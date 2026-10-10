//! CDP Fetch-domain interception that re-validates every browser-issued request
//! against the SSRF policy, closing the gap the pre-navigation seed check leaves
//! open: a browser follows redirects and client-side navigations internally, so
//! without per-request interception a redirect to a private/metadata address
//! would reach the network unchecked. The same interception counts the redirects
//! and the navigations the main frame follows, so `max_redirects` can bound them.
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{BrowserContextId, CloseParams as CloseBrowserParams};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, ContinueResponseParams, DisableParams as FetchDisableParams,
    EnableParams as FetchEnableParams, EventRequestPaused, FailRequestParams, FulfillRequestParams, HeaderEntry,
    RequestId as FetchRequestId, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{
    Cookie, CookieParam, ErrorReason, EventLoadingFailed, Headers, ResourceType, TimeSinceEpoch,
};
use chromiumoxide::cdp::browser_protocol::page::{EventFrameNavigated, EventFrameStoppedLoading, FrameId};
use chromiumoxide::cdp::browser_protocol::storage::{
    GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
};
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, CreateBrowserContextParams, CreateTargetParams, EventTargetCreated, EventTargetDestroyed,
    GetBrowserContextsParams, GetTargetsParams, SetAutoAttachParams, TargetId, TargetInfo,
};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesOrdered, FuturesUnordered, SelectAll, Stream, StreamExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::CrawlError;
use crate::html::is_fetchable_scheme;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::LOGGED_REFUSALS;
use crate::net::credentials::seed_host_headers;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::net::userinfo;
use crate::normalize::resolve_redirect;
use crate::types::CrawlConfig;

/// What an intercepted request is recorded as when it does not parse, so its text is never echoed.
const UNPARSEABLE_URL: &str = "(unparseable URL)";

/// How long closing a watched page and its popups may take before the watch ends anyway.
///
/// ~keep The wait ends at Chrome's destroy events, so the bound only ends the wait of a Chrome
/// ~keep that never sends them. It is longer than `ACTION_SETTLE_LIMIT`, which waits only for the
/// ~keep check's own verdicts, because this waits for Chrome's renderer to let the page go.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROLLER_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// The stop must outlive its bounded context-disposal census before treating the listener as lost. ~keep
const FIREWALL_STOP_TIMEOUT: Duration = Duration::from_secs(6);
const FRAME_ATTRIBUTION_TIMEOUT: Duration = Duration::from_millis(250);
const FRAME_ATTRIBUTION_TARGET_LIMIT: usize = 16;
const CHILD_CONTROLLER_PENDING_LIMIT: usize = 256;
const RETIRED_CONTEXT_LIMIT: usize = 256;
const CHILD_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// The failed URL of that committed document when it is Chrome's error page.
    committed_unreachable_url: Option<String>,
    /// A recorded main-frame document response whose load Chrome reported as failed.
    failed_document: Option<String>,
    /// The network request id of the newest main-frame document request.
    latest_document_request: Option<String>,
    /// The network request id of the main-frame navigation whose redirects are being counted,
    /// and how many it has followed. A redirect of another navigation replaces it, and any
    /// document response clears it.
    pending_redirects: Option<(String, usize)>,
    /// Whether the main frame has received a document that is not a redirect. A redirect past
    /// the limit after it belongs to a navigation the page started, which is dropped instead.
    first_document_arrived: bool,
    /// Redirects the main frame followed: each HTTP redirect, and each navigation after the
    /// first (a meta refresh or a script navigation), counted against the redirect limit.
    redirects_followed: usize,
    /// Whether the main frame has sent the request of its first navigation.
    navigation_started: bool,
    /// Set once the requested navigation is over: later navigations are not counted.
    navigation_ended: bool,
    /// ~keep Whether the check has dropped a main-frame navigation past the redirect limit or one
    /// ~keep whose response cannot commit a document while the requested page is still loading.
    navigation_dropped: bool,
    /// Whether [`Watch::goto`] ended on a frame's stop instead of the page's load.
    goto_unsettled: bool,
}

/// A main-frame response the navigation ends on, reported as it arrived.
///
/// ~keep The headers are read by `browser::navigation`, which needs the `browser` feature;
/// ~keep a `browser-chromiumoxide`-only build has just `interact`, which reads the URL and status.
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct StoppedResponse {
    /// The URL that answered.
    pub(crate) url: String,
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
    pub(crate) body: String,
    pub(crate) body_bytes: Vec<u8>,
    pub(crate) request_id: Option<FetchRequestId>,
    pub(crate) terminal_intercepted: bool,
    pub(crate) ready: bool,
}

impl InterceptOutcome {
    pub(crate) fn take_intentional_terminal(&mut self) -> Option<StoppedResponse> {
        if self.blocked.is_none()
            && self
                .stopped_response
                .as_ref()
                .is_some_and(|response| response.terminal_intercepted)
        {
            self.stopped_response.take()
        } else {
            None
        }
    }
}

impl std::fmt::Debug for StoppedResponse {
    /// Redacted: a sensitive header value, such as a `Set-Cookie`, prints as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url, status, headers, ..
        } = self;
        let url = crate::net::redact_url_credentials(url);
        f.debug_struct("StoppedResponse")
            .field("url", &url)
            .field("status", status)
            .field("headers", &crate::net::redact::RedactedHeaders(headers))
            .finish()
    }
}

/// The status and headers of a main-frame document response.
#[derive(Clone)]
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct DocumentResponse {
    pub(crate) url: String,
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
    /// HTTP redirects the navigation that received this response followed before it.
    pub(crate) redirects: usize,
}

impl std::fmt::Debug for DocumentResponse {
    /// Redacted: a sensitive header value, such as a `Set-Cookie`, prints as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url: _,
            status,
            headers,
            redirects,
        } = self;
        f.debug_struct("DocumentResponse")
            .field("status", status)
            .field("headers", &crate::net::redact::RedactedHeaders(headers))
            .field("redirects", redirects)
            .finish()
    }
}

/// The SSRF check of one chromiumoxide [`Browser`]: a single listener on the browser session
/// that answers every paused request of every target in that browser. Interception is on from
/// the start of the check to its stop, and on a browser that is killed at the end, until the
/// kill. Pages are opened with [`FirewallHandle::new_page`], in the context [`PageContext`]
/// names, and put under the check with [`FirewallHandle::watch`]. Each request is judged by the
/// policy of the watched page it belongs to: the page itself, a frame in it, or a popup it
/// opened, directly or through another popup.
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
    child_controller: tokio::task::JoinHandle<()>,
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
    const fn resolves_target_remotely(self) -> bool {
        match self {
            Self::Launched | Self::Killed => false,
            Self::External => true,
        }
    }

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
    child_commands: mpsc::Sender<ChildCommand>,
    shared: Arc<Shared>,
    /// The browser, held weakly so the owner's `Arc::into_inner` still finds it alone once the
    /// check has stopped.
    browser: Weak<Browser>,
    context: PageContext,
    /// The SSRF proxies this browser's pages go through, one per policy and upstream proxy.
    /// They stop with the check.
    egress: Arc<tokio::sync::Mutex<Vec<crate::net::egress::Egress>>>,
}

/// A page under the check. [`Watch::close`] or [`Watch::park`] ends it; dropping it closes
/// the page like `close`.
pub(crate) struct Watch {
    commands: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
    #[cfg_attr(not(feature = "browser"), allow(dead_code))]
    browser: Weak<Browser>,
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
    /// Wakes navigation when Chrome fails a recorded main-frame document response.
    document_failed: Notify,
    /// Ends the page-scoped event streams when this watch closes or parks.
    events_ended: Notify,
}

/// The targets and frames each watched page owns.
#[derive(Default)]
struct Registry {
    pages: Vec<Arc<WatchedPage>>,
    /// Owner of the browser's default context. It is used only by the process-exclusive
    /// `PageContext::Shared` mode, where Chrome does not expose a BrowserContextId. ~keep
    default_context_owner: Option<Arc<WatchedPage>>,
    shared_context: bool,
    /// Browser contexts created by this firewall and their immutable policy fingerprints.
    /// An unbound owner is opening or parked and therefore remains fail-closed. ~keep
    contexts: HashMap<BrowserContextId, ContextOwner>,
    /// Recently retired context ids, bounded because authoritative absence is the ordering
    /// barrier for older events and known target ids remain in `context_targets`. ~keep
    retired_contexts: VecDeque<BrowserContextId>,
    /// Every target observed in an owned context, retained across park/rewatch handoffs. ~keep
    context_targets: HashMap<TargetId, BrowserContextId>,
    /// The authoritative state of every target the controller has classified. Context identity
    /// is the only path into `Watched`; lineage is used only to detect contradictions. ~keep
    ownership: HashMap<TargetId, TargetOwnership>,
    /// Every live target a watched page owns, in the order they were created: its own and the
    /// popups it opened. A target of an ended watch stays until Chrome destroys it, its
    /// requests refused, and interception stays on until then.
    targets: Vec<(TargetId, Arc<WatchedPage>)>,
    /// ~keep Tabs contain pages and iframes belong to pages; closing either independently
    /// ~keep can destroy a parked root. Keep their policy ownership without popup teardown.
    structural_targets: HashSet<TargetId>,
    /// Every live target no watched page owns: another client's page on an external browser,
    /// or a browser's own tab.
    others: HashSet<TargetId>,
    /// Bounded event-order index for the rare in-process-frame fallback on external pages. ~keep
    other_candidates: Vec<TargetId>,
    /// Identity token for each target's current ownership generation. ~keep
    target_generations: HashMap<TargetId, Arc<()>>,
    /// Frames, keyed by frame id: an in-process frame as a request names it, and a frame Chrome
    /// hosts in a target of its own as that target is created. Each entry names its source target
    /// and ownership generation, so reuse cannot retain the preceding policy. ~keep
    frames: HashMap<FrameId, FrameOwner>,
    /// Every page the check opened, by its own target, with the browser context it lives in when
    /// it has one of its own. The page goes when its watch ends with it, when Chrome destroys it,
    /// or when the check stops; a context of its own, and its popups, go with it.
    opened: HashMap<TargetId, Option<BrowserContextId>>,
}

#[derive(Clone)]
enum TargetOwnership {
    Watched(Arc<WatchedPage>),
    PendingContext(BrowserContextId),
    /// A target in the process-exclusive default context before its watch is installed. ~keep
    PendingShared,
    Other,
    /// An owned or lineage-related target whose authoritative record is missing or conflicts.
    /// Its requests are always refused and teardown continues to account for it. ~keep
    Quarantined,
}

struct ContextOwner {
    policy: Option<PolicyIdentity>,
    owner: Option<Arc<WatchedPage>>,
    disposal_acknowledged: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct PolicyIdentity {
    deny_private: bool,
    allowlist: Vec<crate::net::ssrf::HostMatcher>,
    denylist: Vec<crate::net::ssrf::HostMatcher>,
    max_redirects: u8,
    scheme_allowlist: Vec<String>,
}

impl From<&crate::net::ssrf::SsrfPolicy> for PolicyIdentity {
    fn from(policy: &crate::net::ssrf::SsrfPolicy) -> Self {
        Self {
            deny_private: policy.deny_private,
            allowlist: policy.allowlist.clone(),
            denylist: policy.denylist.clone(),
            max_redirects: policy.max_redirects,
            scheme_allowlist: policy.scheme_allowlist.clone(),
        }
    }
}

#[derive(Clone)]
struct FrameOwner {
    source: TargetId,
    generation: Arc<()>,
    owner: Option<Arc<WatchedPage>>,
}

/// A paused request, counted from the pause until it is matched to the page that sent it, so a
/// wait can see a request that belongs to no page yet. Dropping it releases the count, so a
/// listener that stops mid-match leaves none behind.
struct Paused<'a> {
    shared: &'a Shared,
    at: Instant,
}

impl<'a> Paused<'a> {
    fn new(shared: &'a Shared, at: Instant) -> Self {
        lock(&shared.unmatched).push(at);
        Self { shared, at }
    }

    /// Hand the count to `page`. The page counts the request before the pause lets go of it,
    /// so no wait can see it counted nowhere.
    fn matched(self, page: Arc<WatchedPage>) -> InFlight {
        let in_flight = InFlight::enter(page);
        drop(self);
        in_flight
    }
}

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        let mut unmatched = lock(&self.shared.unmatched);
        if let Some(index) = unmatched.iter().position(|at| *at == self.at) {
            unmatched.swap_remove(index);
        }
    }
}

/// Who a paused request belongs to.
enum Owner {
    Watched(Arc<WatchedPage>),
    Other,
}

impl Registry {
    fn register_context(&mut self, context: BrowserContextId, policy: Option<&crate::net::ssrf::SsrfPolicy>) {
        self.retired_contexts.retain(|retired| retired != &context);
        self.contexts.insert(
            context,
            ContextOwner {
                policy: policy.map(PolicyIdentity::from),
                owner: None,
                disposal_acknowledged: false,
            },
        );
    }

    fn owner_of_target(&self, target: &str) -> Option<Arc<WatchedPage>> {
        match self.ownership.get(&TargetId::new(target)) {
            Some(TargetOwnership::Watched(page)) => Some(Arc::clone(page)),
            _ => None,
        }
    }

    fn owner_of_frame(&self, frame: &FrameId) -> Option<Owner> {
        let owner = self.frame_owner(frame)?.owner;
        Some(owner.map_or(Owner::Other, Owner::Watched))
    }

    fn frame_owner(&self, frame: &FrameId) -> Option<FrameOwner> {
        let target = TargetId::new(frame.inner());
        if let Some(page) = self.owner_of_target(frame.inner()) {
            return self.current_frame_owner(target, Some(page));
        }
        if self.others.contains(&target) {
            return self.current_frame_owner(target, None);
        }
        self.frames
            .get(frame)
            .filter(|cached| {
                self.target_generations
                    .get(&cached.source)
                    .is_some_and(|current| Arc::ptr_eq(current, &cached.generation))
            })
            .cloned()
    }

    fn current_frame_owner(&self, source: TargetId, owner: Option<Arc<WatchedPage>>) -> Option<FrameOwner> {
        Some(FrameOwner {
            generation: Arc::clone(self.target_generations.get(&source)?),
            source,
            owner,
        })
    }

    fn register_watched(&mut self, target: TargetId, owner: Arc<WatchedPage>) {
        self.invalidate_frames_from(&target);
        self.others.remove(&target);
        self.targets.retain(|(id, _)| *id != target);
        self.target_generations.insert(target.clone(), Arc::new(()));
        if !self.structural_targets.contains(&target) {
            self.targets.push((target.clone(), Arc::clone(&owner)));
        }
        self.ownership.insert(target, TargetOwnership::Watched(owner));
    }

    fn register_other(&mut self, target: TargetId) {
        // ~keep A creation event queued before `Watch` can arrive after it; it must not demote
        // ~keep the target whose watch is already installed to an unrestricted external target.
        if self.targets.iter().any(|(id, _)| *id == target)
            || self.others.contains(&target)
            || self.context_targets.contains_key(&target)
            || self.ownership.contains_key(&target)
        {
            return;
        }
        self.invalidate_frames_from(&target);
        self.others.insert(target.clone());
        self.ownership.insert(target.clone(), TargetOwnership::Other);
        if self.other_candidates.len() < FRAME_ATTRIBUTION_TARGET_LIMIT {
            self.other_candidates.push(target.clone());
        }
        self.target_generations.insert(target, Arc::new(()));
    }

    fn register_pending(&mut self, target: TargetId, context: BrowserContextId) {
        self.invalidate_frames_from(&target);
        self.others.remove(&target);
        self.targets.retain(|(id, _)| *id != target);
        self.target_generations.insert(target.clone(), Arc::new(()));
        self.context_targets.insert(target.clone(), context.clone());
        self.ownership.insert(target, TargetOwnership::PendingContext(context));
    }

    fn register_pending_shared(&mut self, target: TargetId) {
        self.invalidate_frames_from(&target);
        self.others.remove(&target);
        self.targets.retain(|(id, _)| *id != target);
        self.target_generations.insert(target.clone(), Arc::new(()));
        self.ownership.insert(target, TargetOwnership::PendingShared);
    }

    fn remove_target(&mut self, target: &TargetId) {
        self.invalidate_frames_from(target);
        self.targets.retain(|(id, _)| id != target);
        self.others.remove(target);
        self.context_targets.remove(target);
        self.ownership.remove(target);
        self.structural_targets.remove(target);
        self.other_candidates.retain(|candidate| candidate != target);
        self.target_generations.remove(target);
    }

    fn invalidate_frames_from(&mut self, target: &TargetId) {
        self.frames
            .retain(|frame, cached| frame.inner() != target.inner() && cached.source != *target);
    }

    fn register_quarantined(&mut self, target: TargetId, context: Option<BrowserContextId>) {
        self.invalidate_frames_from(&target);
        self.others.remove(&target);
        self.targets.retain(|(id, _)| *id != target);
        match &context {
            Some(context) => {
                self.context_targets.insert(target.clone(), context.clone());
            }
            None => {
                self.context_targets.remove(&target);
            }
        }
        self.target_generations.insert(target.clone(), Arc::new(()));
        self.ownership.insert(target, TargetOwnership::Quarantined);
    }

    fn retire_context(&mut self, context: &BrowserContextId) -> bool {
        let Some(registered) = self.contexts.get(context) else {
            return false;
        };
        if !registered.disposal_acknowledged {
            return false;
        }
        self.contexts.remove(context);
        self.retired_contexts.retain(|retired| retired != context);
        self.retired_contexts.push_back(context.clone());
        if self.retired_contexts.len() > RETIRED_CONTEXT_LIMIT {
            self.retired_contexts.pop_front();
        }
        self.targets
            .retain(|(target, _)| self.context_targets.get(target) != Some(context));
        self.frames
            .retain(|_, frame| self.context_targets.get(&frame.source) != Some(context));
        for (target, ownership) in &mut self.ownership {
            if self.context_targets.get(target) == Some(context) {
                *ownership = TargetOwnership::Quarantined;
            }
        }
        true
    }

    fn owns_live_target(&self, page: &Arc<WatchedPage>, keep_root: bool) -> bool {
        self.targets
            .iter()
            .any(|(id, owner)| Arc::ptr_eq(owner, page) && !(keep_root && *id == page.root))
    }
}

/// The exact context registered for `root`. An entry containing `None` is the browser's shared
/// context; no entry is an ownership failure and must never fall back to that shared jar. ~keep
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
fn registered_context(registry: &Registry, root: &TargetId) -> Result<Option<BrowserContextId>, CrawlError> {
    registry
        .opened
        .get(root)
        .cloned()
        .ok_or_else(|| CrawlError::browser_error("watched page has no registered browser context"))
}

struct Shared {
    registry: Mutex<Registry>,
    /// Notified whenever a target is destroyed.
    destroyed: Notify,
    origin: BrowserOrigin,
    context: PageContext,
    /// Every request Chrome has paused that is not matched to a page yet, by the time the
    /// listener received its pause. The match can take longer than the grace after an action,
    /// so a wait on the page's own count alone misses such a request: see [`Watch::settle`].
    unmatched: Mutex<Vec<Instant>>,
    /// Page creation is detached from its caller so cancellation cannot leak a context. ~keep
    opening: AtomicUsize,
    stopping: AtomicBool,
    failed: AtomicBool,
    failure: Notify,
    handler_abort: Option<tokio::task::AbortHandle>,
    #[cfg(test)]
    delays: TestDelays,
}

struct Opening(Arc<Shared>);

impl Opening {
    fn begin(shared: &Arc<Shared>) -> Result<Self, CrawlError> {
        let stopped = || CrawlError::browser_error("request interception stopped");
        if shared.stopping.load(Ordering::Acquire) {
            return Err(stopped());
        }
        shared.opening.fetch_add(1, Ordering::AcqRel);
        if shared.stopping.load(Ordering::Acquire) {
            shared.opening.fetch_sub(1, Ordering::AcqRel);
            return Err(stopped());
        }
        Ok(Self(Arc::clone(shared)))
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.0.opening.fetch_sub(1, Ordering::AcqRel);
    }
}

struct WatchSetup {
    commands: mpsc::UnboundedSender<Command>,
    root: TargetId,
    armed: bool,
}

impl WatchSetup {
    fn new(commands: &mpsc::UnboundedSender<Command>, root: TargetId) -> Self {
        Self {
            commands: commands.clone(),
            root,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WatchSetup {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.commands.send(Command::Abandon(self.root.clone()));
        }
    }
}

/// Delays and faults a unit test injects to widen a race window deterministically.
#[cfg(test)]
#[derive(Clone, Default)]
struct TestDelays {
    /// Before a request's SSRF verdict, as a slow DNS lookup would take.
    verdict: Duration,
    /// Between a request's verdict and the answer that delivers it to Chrome.
    deliver: Duration,
    /// Before a watch's end closes a target, as Chrome under load is slow to destroy one.
    close: Duration,
    /// Between a request's verdict and its delivery, until the test gives the gate a permit.
    deliver_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Before the listener takes in a paused request, until the test gives the gate a permit.
    receive_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Before a request's SSRF verdict, until the test gives the gate a permit.
    verdict_gate: Option<Arc<UrlGate>>,
    /// Before a paused request whose URL starts with the prefix is matched to the page that sent
    /// it, as the frame-tree lookup of a frame the registry does not know yet holds it. Each permit
    /// the test gives lets one request through.
    match_gate: Option<(Arc<UrlGate>, String)>,
    /// Between the verdict and the delivery of a request that belongs to no watched page, until
    /// the test gives the gate a permit. Such a request waits on this gate in place of the
    /// delivery gate.
    unwatched_deliver_gate: Option<Arc<UrlGate>>,
    /// The URLs of the paused requests the listener has taken in, recorded as each is taken in.
    received: Arc<Mutex<Vec<String>>>,
    /// The pages the listener has dropped, recorded as each drop starts.
    dropped: Arc<Mutex<Vec<TargetId>>>,
    /// ~keep Skip the socket-level SSRF proxy so a negative control isolates Fetch interception.
    bypass_egress: bool,
    /// ~keep Per action, its attribution grace and subsequent request-settling poll count.
    action_waits: Arc<Mutex<Vec<(Duration, usize)>>>,
    /// Before a watch hands its policy to the listener, until the test gives one permit. ~keep
    watch_handoff_gate: Option<Arc<UrlGate>>,
    /// After frame attribution snapshots target ownership, until the test gives one permit. ~keep
    attribute_snapshot_gate: Option<Arc<UrlGate>>,
    /// Before a lifecycle reconciliation queries Chrome, until the test gives one permit. ~keep
    lifecycle_query_gate: Option<Arc<UrlGate>>,
}

#[cfg(test)]
tokio::task_local! {
    static CALL_SITE_DELAYS: TestDelays;
}

#[cfg(test)]
pub(crate) async fn with_recorded_action_waits<F: std::future::Future>(body: F) -> (F::Output, Vec<(Duration, usize)>) {
    let action_waits = Arc::new(Mutex::new(Vec::new()));
    let delays = TestDelays {
        action_waits: Arc::clone(&action_waits),
        ..TestDelays::default()
    };
    let output = CALL_SITE_DELAYS.scope(delays, body).await;
    let recorded = lock(&action_waits).clone();
    (output, recorded)
}

/// Holds each request at one step of its answer until the test gives a permit, and lists the URLs
/// it has held, so a test knows which request the listener has taken in.
#[cfg(test)]
struct UrlGate {
    permits: tokio::sync::Semaphore,
    held: Mutex<Vec<String>>,
}

enum Command {
    /// The check opened a page: its own target, and its browser context when it has one of its
    /// own.
    Opened(TargetId, Option<BrowserContextId>),
    /// Watch the page, recording main-frame commits and document load failures.
    Watch(
        Arc<WatchedPage>,
        chromiumoxide::listeners::EventStream<EventFrameNavigated>,
        chromiumoxide::listeners::EventStream<EventLoadingFailed>,
        oneshot::Sender<Result<(), String>>,
    ),
    /// Cancellation cannot run async cleanup, so it hands disposal to the listener. ~keep
    Abandon(TargetId),
    /// Close the page's popups, and the page itself when `close_page` is set, then stop
    /// watching it.
    End {
        page: Arc<WatchedPage>,
        close_page: bool,
        done: Option<oneshot::Sender<()>>,
    },
    /// Drop every page of the check that is left, turn interception off (unless the pages
    /// share the browser's context or the browser is to be killed) once that and every answer
    /// and every watch end already started have finished, and on an external browser once every
    /// target of a closed page is destroyed, then stop the listener. `done` is told when the
    /// listener stops.
    Stop(Option<oneshot::Sender<()>>),
}

enum ChildCommand {
    Enable {
        done: oneshot::Sender<Result<(), String>>,
    },
    Arm {
        root: TargetId,
        done: oneshot::Sender<Result<(), String>>,
    },
    Stop,
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
    #[cfg(test)]
    pub(crate) async fn start(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
    ) -> Result<Self, CrawlError> {
        Self::start_with_supervisor(
            browser,
            origin,
            context,
            None,
            #[cfg(test)]
            CALL_SITE_DELAYS.try_with(TestDelays::clone).unwrap_or_default(),
        )
        .await
    }

    pub(crate) async fn start_supervised(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
        handler_abort: tokio::task::AbortHandle,
    ) -> Result<Self, CrawlError> {
        Self::start_with_supervisor(
            browser,
            origin,
            context,
            Some(handler_abort),
            #[cfg(test)]
            CALL_SITE_DELAYS.try_with(TestDelays::clone).unwrap_or_default(),
        )
        .await
    }

    #[cfg(test)]
    async fn start_with(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
        #[cfg(test)] delays: TestDelays,
    ) -> Result<Self, CrawlError> {
        Self::start_with_supervisor(
            browser,
            origin,
            context,
            None,
            #[cfg(test)]
            delays,
        )
        .await
    }

    async fn start_with_supervisor(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
        handler_abort: Option<tokio::task::AbortHandle>,
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
        let existing = match browser.execute(GetTargetsParams::default()).await {
            Ok(response) => response.result.target_infos,
            Err(error) if origin == BrowserOrigin::External => {
                return Err(CrawlError::browser_error(format!(
                    "failed to enumerate existing external-browser targets: {error}"
                )));
            }
            Err(error) => {
                tracing::debug!(%error, "failed to enumerate targets before enabling browser interception");
                Vec::new()
            }
        };
        browser
            .execute(fetch_enable_params())
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;
        let (child_socket, _) = tokio::time::timeout(
            CONTROLLER_TEARDOWN_TIMEOUT,
            async_tungstenite::tokio::connect_async(browser.websocket_address().as_str()),
        )
        .await
        .map_err(|_| CrawlError::browser_error("timed out starting the child-target controller"))?
        .map_err(|e| CrawlError::browser_error(format!("failed to start the child-target controller: {e}")))?;
        let mut registry = Registry {
            shared_context: context == PageContext::Shared,
            ..Registry::default()
        };
        for info in existing {
            registry.register_other(info.target_id);
        }
        let shared = Arc::new(Shared {
            registry: Mutex::new(registry),
            destroyed: Notify::new(),
            origin,
            context,
            unmatched: Mutex::new(Vec::new()),
            opening: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            failure: Notify::new(),
            handler_abort,
            #[cfg(test)]
            delays,
        });
        let (commands, receiver) = mpsc::unbounded_channel();
        let (child_commands, child_receiver) = mpsc::channel(CHILD_CONTROLLER_PENDING_LIMIT);
        let handle = FirewallHandle {
            commands,
            child_commands: child_commands.clone(),
            shared: Arc::clone(&shared),
            browser: Arc::downgrade(&browser),
            context,
            egress: Arc::default(),
        };
        let child_shared = Arc::clone(&shared);
        let child_browser = Arc::clone(&browser);
        let child_controller = tokio::spawn(async move {
            let expected = controller_completed(serve_child_targets(child_socket, child_receiver, &child_shared)).await;
            if !expected && !child_shared.stopping.load(Ordering::Acquire) {
                fail_controller(&child_browser, &child_shared, "child-target controller").await;
            }
        });
        let (enabled, ready) = oneshot::channel();
        child_commands
            .send(ChildCommand::Enable { done: enabled })
            .await
            .map_err(|_| CrawlError::browser_error("child-target controller stopped before it could be enabled"))?;
        tokio::time::timeout(CHILD_COMMAND_TIMEOUT, ready)
            .await
            .map_err(|_| CrawlError::browser_error("timed out enabling the child-target controller"))?
            .map_err(|_| CrawlError::browser_error("child-target controller stopped before it could be enabled"))?
            .map_err(CrawlError::browser_error)?;
        let listener_shared = Arc::clone(&shared);
        let listener = tokio::spawn(async move {
            let expected = controller_completed(serve(
                Arc::clone(&browser),
                Arc::clone(&listener_shared),
                Events {
                    paused,
                    created,
                    destroyed,
                },
                receiver,
            ))
            .await;
            if expected {
                return;
            }
            fail_controller(&browser, &listener_shared, "request-interception controller").await;
        });
        Ok(Self {
            handle,
            listener,
            child_controller,
            stopped: false,
        })
    }

    pub(crate) fn handle(&self) -> FirewallHandle {
        self.handle.clone()
    }

    pub(crate) fn has_failed(&self) -> bool {
        self.handle.shared.failed.load(Ordering::Acquire)
    }

    /// Drop every page of the check that is left, so no page of it can still send, turn
    /// interception off once that and every answer the check had started when asked to stop are
    /// done, stop the listener, and release its reference to the browser, so the owner can close
    /// it. On a [`BrowserOrigin::Killed`] browser interception is left on. Call it once no page of the browser needs the check any more.
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
        self.handle.shared.stopping.store(true, Ordering::Release);
        let (done, stopped) = oneshot::channel();
        let completed = if self.handle.commands.send(Command::Stop(Some(done))).is_ok() {
            tokio::time::timeout(FIREWALL_STOP_TIMEOUT, stopped).await.is_ok()
        } else {
            true
        };
        if !completed {
            self.handle.shared.failed.store(true, Ordering::Release);
            self.handle.shared.failure.notify_waiters();
            tracing::error!("browser request-interception teardown exceeded its bound");
            if self.handle.shared.origin != BrowserOrigin::External
                && let Some(browser) = self.handle.browser.upgrade()
            {
                let _ = tokio::time::timeout(
                    CONTROLLER_TEARDOWN_TIMEOUT,
                    browser.execute(CloseBrowserParams::default()),
                )
                .await;
            }
            if let Some(abort) = &self.handle.shared.handler_abort {
                abort.abort();
            }
            self.listener.abort();
        }
        self.stopped = true;
        if tokio::time::timeout(CONTROLLER_TEARDOWN_TIMEOUT, &mut self.listener)
            .await
            .is_err()
        {
            self.handle.shared.failed.store(true, Ordering::Release);
            self.handle.shared.failure.notify_waiters();
            tracing::error!("browser request-interception controller did not terminate within its bound");
            if let Some(abort) = &self.handle.shared.handler_abort {
                abort.abort();
            }
            self.listener.abort();
        }
        let _ = self.handle.child_commands.send(ChildCommand::Stop).await;
        if tokio::time::timeout(CONTROLLER_TEARDOWN_TIMEOUT, &mut self.child_controller)
            .await
            .is_err()
        {
            self.handle.shared.failed.store(true, Ordering::Release);
            self.handle.shared.failure.notify_waiters();
            tracing::error!("child-target controller did not terminate within its bound");
            self.child_controller.abort();
        }
    }
}

async fn fail_controller(browser: &Browser, shared: &Shared, controller: &'static str) {
    if shared.failed.swap(true, Ordering::AcqRel) {
        return;
    }
    shared.failure.notify_waiters();
    shared.stopping.store(true, Ordering::Release);
    tracing::error!(controller, "browser security controller ended unexpectedly");
    if shared.origin != BrowserOrigin::External {
        let _ = tokio::time::timeout(
            CONTROLLER_TEARDOWN_TIMEOUT,
            browser.execute(CloseBrowserParams::default()),
        )
        .await;
    }
    if let Some(abort) = &shared.handler_abort {
        abort.abort();
    }
}

impl Drop for BrowserFirewall {
    // ~keep The stop repeats `stop`'s for the path that never reaches it: a cancelled fetch
    // ~keep future drops the firewall without stopping it, and interception left on with no
    // ~keep listener pauses the whole browser. The listener keeps answering until its authoritative
    // ~keep census proves every owned target is gone, then ends and lets go of the browser. External
    // ~keep sessions lose interception on detach; launched and killed browsers retain it until exit.
    fn drop(&mut self) {
        if !self.stopped {
            self.handle.shared.stopping.store(true, Ordering::Release);
            let _ = self.handle.commands.send(Command::Stop(None));
            let _ = self.handle.child_commands.try_send(ChildCommand::Stop);
        }
    }
}

/// Drop the page `root` the check opened: dispose its browser context `context` when it has one
/// of its own, and with it every page in it and every request of theirs Chrome still holds;
/// otherwise close the page's target.
///
/// ~keep Closing a target under browser-wide interception lets none of its pending requests
/// ~keep out (measured: 854 to 6674 pauses outstanding at the close, 0 reached, 3 of 3 runs);
/// ~keep what takes them out is turning interception off, which a shared-context check never does.
async fn drop_page(browser: &Browser, shared: &Shared, root: TargetId, context: Option<BrowserContextId>) {
    match context {
        Some(context) => {
            let _ = dispose_and_retire_context(browser, shared, context).await;
        }
        None => {
            if let Err(error) = browser.execute(CloseTargetParams::new(root)).await {
                tracing::debug!(%error, "failed to close a page of the check");
            }
        }
    }
}

/// Dispose the browser context `context`, and with it every page in it and every request of
/// theirs Chrome still holds.
async fn dispose_context(browser: &Browser, context: BrowserContextId) -> bool {
    if let Err(error) = browser.dispose_browser_context(context).await {
        tracing::debug!(%error, "failed to dispose a page's browser context");
        return false;
    }
    true
}

async fn context_absent(browser: &Browser, context: &BrowserContextId) -> bool {
    let Ok(contexts) = browser.execute(GetBrowserContextsParams::default()).await else {
        return false;
    };
    if contexts.result.browser_context_ids.contains(context) {
        return false;
    }
    let Ok(targets) = browser.execute(GetTargetsParams::default()).await else {
        return false;
    };
    !targets
        .result
        .target_infos
        .iter()
        .any(|target| target.browser_context_id.as_ref() == Some(context))
}

async fn dispose_and_retire_context(browser: &Browser, shared: &Shared, context: BrowserContextId) -> bool {
    if !dispose_context(browser, context.clone()).await {
        return false;
    }
    if let Some(registered) = lock(&shared.registry).contexts.get_mut(&context) {
        registered.disposal_acknowledged = true;
    }
    let absent = tokio::time::timeout(CLOSE_TIMEOUT, async {
        loop {
            if context_absent(browser, &context).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .is_ok();
    if absent {
        let _ = lock(&shared.registry).retire_context(&context);
    }
    absent
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

async fn open_page(
    browser: &Browser,
    shared: &Shared,
    commands: &mpsc::UnboundedSender<Command>,
    mode: PageContext,
    proxy_server: Option<String>,
    policy: Option<crate::net::ssrf::SsrfPolicy>,
) -> Result<(chromiumoxide::Page, Option<BrowserContextId>), CrawlError> {
    let stopped = || CrawlError::browser_error("request interception stopped");
    let failed = |error: &dyn std::fmt::Display| CrawlError::browser_error(format!("failed to create page: {error}"));
    let context = match mode {
        PageContext::Shared => None,
        PageContext::Isolated | PageContext::Copied => Some(
            browser
                .create_browser_context(CreateBrowserContextParams {
                    dispose_on_detach: Some(true),
                    proxy_bypass_list: proxy_server
                        .as_ref()
                        .map(|_| crate::browser_pool::NO_LOOPBACK_BYPASS.to_owned()),
                    proxy_server,
                    ..CreateBrowserContextParams::default()
                })
                .await
                .map_err(|error| failed(&error))?,
        ),
    };
    if let Some(context) = &context {
        lock(&shared.registry).register_context(context.clone(), policy.as_ref());
    }
    if mode == PageContext::Copied
        && let Some(context) = &context
        && let Err(error) = copy_cookies(browser, context.clone()).await
    {
        let _ = dispose_and_retire_context(browser, shared, context.clone()).await;
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
                let _ = dispose_and_retire_context(browser, shared, context).await;
            }
            return Err(failed(&error));
        }
    };
    let root = page.target_id().clone();
    if commands.send(Command::Opened(root.clone(), context.clone())).is_err() {
        drop_page(browser, shared, root, context).await;
        return Err(stopped());
    }
    Ok((page, context))
}

impl FirewallHandle {
    pub(crate) async fn failed(&self) {
        loop {
            let notified = self.shared.failure.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.shared.failed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// The SSRF proxy for `policy` leaving through `upstream`, started on first use, or none
    /// when the policy has no IP-level denials.
    async fn egress_proxy(
        &self,
        upstream: Option<&crate::proxy::ChromeProxy>,
        policy: &crate::net::ssrf::SsrfPolicy,
    ) -> Result<Option<crate::proxy::ChromeProxy>, CrawlError> {
        let mut running = self.egress.lock().await;
        if let Some(egress) = running.iter().find(|egress| egress.serves(policy, upstream)) {
            return Ok(Some(egress.chrome_proxy()));
        }
        let Some(egress) = crate::net::egress::Egress::start(policy, upstream).await? else {
            return Ok(None);
        };
        let proxy = egress.chrome_proxy();
        running.push(egress);
        Ok(Some(proxy))
    }

    /// Every `host:port` this browser's SSRF proxies refused, in the order of each proxy.
    pub(crate) async fn egress_refused(&self) -> Vec<String> {
        self.egress
            .lock()
            .await
            .iter()
            .flat_map(crate::net::egress::Egress::refused)
            .collect()
    }

    /// Open a blank page for [`Self::watch`], in a browser context of its own or in the
    /// browser's, as the check's [`PageContext`] says. A context of its own starts with the
    /// browser's cookies when the check copies them. The page goes, with its popups, when its
    /// watch ends with it, when Chrome destroys it, or when the check stops.
    ///
    /// ~keep A created context is disposed with the debugging session too, so a check that ends
    /// ~keep without stopping (a crashed process) leaves no context in a `browser.endpoint` Chrome.
    ///
    /// With `proxy`, the page's own browser context is made with it, so its requests go through
    /// it. With `sockets` under `deny_private` the context goes through the SSRF proxy instead,
    /// which leaves through `proxy`. The browser's own context has no proxy of its own: a
    /// `PageContext::Shared` page uses the proxy the browser was launched with.
    ///
    /// ~keep Crawlberg's chromiumoxide fork leaves child resumption to the raw controller. That
    /// ~keep controller arms the structural tab and its context-bearing page before either can
    /// ~keep resume; browser-wide Fetch is already enabled, and any exact-policy egress proxy is
    /// ~keep installed on the context before its first target exists. External Chrome is rejected
    /// ~keep when its socket boundary cannot enforce the configured policy.
    pub(crate) async fn new_page_with_policy(
        &self,
        proxy: Option<&crate::proxy::ChromeProxy>,
        sockets: Option<&crate::net::ssrf::SsrfPolicy>,
        policy: &crate::net::ssrf::SsrfPolicy,
    ) -> Result<chromiumoxide::Page, CrawlError> {
        self.new_page_inner(proxy, sockets, Some(policy)).await
    }

    #[cfg(test)]
    pub(crate) async fn new_page(
        &self,
        proxy: Option<&crate::proxy::ChromeProxy>,
        sockets: Option<&crate::net::ssrf::SsrfPolicy>,
    ) -> Result<chromiumoxide::Page, CrawlError> {
        self.new_page_inner(proxy, sockets, None).await
    }

    async fn new_page_inner(
        &self,
        proxy: Option<&crate::proxy::ChromeProxy>,
        sockets: Option<&crate::net::ssrf::SsrfPolicy>,
        policy: Option<&crate::net::ssrf::SsrfPolicy>,
    ) -> Result<chromiumoxide::Page, CrawlError> {
        let stopped = || CrawlError::browser_error("request interception stopped");
        let browser = self.browser.upgrade().ok_or_else(stopped)?;
        #[cfg(test)]
        let sockets = sockets.filter(|_| !self.shared.delays.bypass_egress);
        let egress = match (&self.context, sockets) {
            (PageContext::Isolated | PageContext::Copied, Some(policy)) => self.egress_proxy(proxy, policy).await?,
            _ => None,
        };
        let proxy_server = egress.as_ref().or(proxy).map(|proxy| proxy.server.clone());
        let opening = Opening::begin(&self.shared)?;
        let shared = Arc::clone(&self.shared);
        let commands = self.commands.clone();
        let mode = self.context;
        let policy = policy.cloned();
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let _opening = opening;
            let opened = open_page(&browser, &shared, &commands, mode, proxy_server, policy).await;
            if let Err(Ok((page, context))) = reply.send(opened) {
                drop_page(&browser, &shared, page.target_id().clone(), context).await;
            }
        });
        let (page, _) = result.await.map_err(|_| stopped())??;
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
        let mut setup = WatchSetup::new(&self.commands, page.target_id().clone());
        let main_frame = require_main_frame(page.mainframe().await.map_err(|e| e.to_string()))?;
        let navigations = page
            .event_listener::<EventFrameNavigated>()
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to register navigation listener: {e}")))?;
        let failures = page
            .event_listener::<EventLoadingFailed>()
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to register document failure listener: {e}")))?;
        #[cfg(test)]
        if let Some(gate) = &self.shared.delays.watch_handoff_gate {
            lock(&gate.held).push(page.target_id().inner().clone());
            if let Ok(permit) = gate.permits.acquire().await {
                permit.forget();
            }
        }
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
            document_failed: Notify::new(),
            events_ended: Notify::new(),
        });
        let (ack, enabled) = oneshot::channel();
        self.commands
            .send(Command::Watch(Arc::clone(&watched), navigations, failures, ack))
            .map_err(|_| CrawlError::browser_error("request interception stopped"))?;
        let watch = Watch {
            commands: self.commands.clone(),
            shared: Arc::clone(&self.shared),
            browser: self.browser.clone(),
            page: watched,
            ended: false,
        };
        match enabled.await {
            Ok(Ok(())) => {
                disable_primary_child_attach(page).await?;
                let (armed, boundary) = oneshot::channel();
                self.child_commands
                    .send(ChildCommand::Arm {
                        root: page.target_id().clone(),
                        done: armed,
                    })
                    .await
                    .map_err(|_| CrawlError::browser_error("child-target controller stopped"))?;
                boundary
                    .await
                    .map_err(|_| CrawlError::browser_error("child-target controller stopped"))?
                    .map_err(CrawlError::browser_error)?;
                setup.disarm();
                Ok(watch)
            }
            Ok(Err(e)) => Err(CrawlError::browser_error(e)),
            Err(_) => Err(CrawlError::browser_error("request interception stopped")),
        }
    }
}

async fn disable_primary_child_attach(page: &chromiumoxide::Page) -> Result<(), CrawlError> {
    page.execute(SetAutoAttachParams {
        auto_attach: false,
        wait_for_debugger_on_start: false,
        flatten: Some(true),
        filter: None,
    })
    .await
    .map(drop)
    .map_err(|e| CrawlError::browser_error(format!("failed to disable the primary child-target controller: {e}")))
}

impl Watch {
    /// The browser context this watched page owns, or `None` when it uses the browser's shared
    /// context. Storage-domain cookie commands need this because they run at browser scope. ~keep
    #[cfg_attr(not(feature = "browser"), allow(dead_code))]
    pub(crate) fn cookie_store(&self) -> Result<(Arc<Browser>, Option<BrowserContextId>), CrawlError> {
        let context = registered_context(&lock(&self.shared.registry), &self.page.root)?;
        let browser = self
            .browser
            .upgrade()
            .ok_or_else(|| CrawlError::browser_error("browser closed before its cookie jar could be accessed"))?;
        Ok((browser, context))
    }

    /// Return how the navigation went so far, and keep watching: the first blocked request and
    /// the response the navigation stopped on.
    pub(crate) fn take_outcome(&self) -> InterceptOutcome {
        let mut state = lock(&self.page.outcome);
        let stopped_response = if state.stopped_response.as_ref().is_some_and(|response| response.ready) {
            state.stopped_response.take()
        } else {
            None
        };
        InterceptOutcome {
            blocked: state.blocked.take(),
            stopped_response,
            ..InterceptOutcome::default()
        }
    }

    pub(crate) fn take_stopped_response(&self) -> Option<StoppedResponse> {
        let mut outcome = lock(&self.page.outcome);
        if outcome.stopped_response.as_ref().is_some_and(|response| response.ready) {
            outcome.stopped_response.take()
        } else {
            None
        }
    }

    pub(crate) async fn take_stopped_response_within(
        &self,
        timeout: Duration,
    ) -> Result<Option<StoppedResponse>, CrawlError> {
        let pending = || lock(&self.page.outcome).stopped_response.is_some();
        if !pending() {
            return Ok(None);
        }
        match tokio::time::timeout(timeout, async {
            loop {
                if let Some(response) = self.take_stopped_response() {
                    return response;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        {
            Ok(response) => Ok(Some(response)),
            Err(_) => {
                self.mark_unsettled();
                Err(CrawlError::browser_timeout(format!(
                    "browser timed out after {timeout:?} reading a terminal response"
                )))
            }
        }
    }

    /// The redirects the main frame has followed since the watch began: HTTP redirects, and
    /// the navigations after the first.
    #[cfg(feature = "browser")]
    pub(crate) fn redirects_followed(&self) -> usize {
        lock(&self.page.outcome).redirects_followed
    }

    /// Navigate the watched page to `url` and wait for it to load, as `Page::goto` does.
    ///
    /// ~keep A navigation the check drops leaves chromiumoxide 0.9.1's `goto` waiting for a load
    /// ~keep event that never comes. Chrome reports the dropped navigation's start, which clears
    /// ~keep the load chromiumoxide recorded, and commits no document after it. A navigation a
    /// ~keep script starts while the page is parsing also stops the page's parser, so the page
    /// ~keep never fires its own load. Chrome still sends `Page.frameStoppedLoading` once a
    /// ~keep frame is idle, and chromiumoxide ignores that event, so once a navigation was dropped
    /// ~keep the navigation also ends on the first frame that stops. Which frame does not matter:
    /// ~keep no document commits after the drop, so the page keeps the one it has.
    ///
    /// ~keep Chrome's error page can commit without firing the requested page's load event. Its
    /// ~keep `frameNavigated` event carries `unreachable_url`, so that commit ends the wait too;
    /// ~keep the caller then classifies the recorded response instead of reporting a timeout.
    pub(crate) async fn goto(
        &self,
        page: &chromiumoxide::Page,
        url: &str,
    ) -> Result<(), chromiumoxide::error::CdpError> {
        let mut stops = page.event_listener::<EventFrameStoppedLoading>().await?;
        let mut commits = page.event_listener::<EventFrameNavigated>().await?;
        let stopped_after_a_drop = async {
            while stops.next().await.is_some() {
                if lock(&self.page.outcome).navigation_dropped {
                    return;
                }
            }
            std::future::pending::<()>().await;
        };
        let main_frame = self.page.main_frame.clone();
        let error_page_committed = async {
            while let Some(event) = commits.next().await {
                if event.frame.id == main_frame && event.frame.unreachable_url.is_some() {
                    return event;
                }
            }
            std::future::pending::<Arc<EventFrameNavigated>>().await
        };
        let document_failed = async {
            loop {
                let notified = self.page.document_failed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if lock(&self.page.outcome).failed_document.is_some() {
                    return;
                }
                notified.await;
            }
        };
        tokio::pin!(error_page_committed);
        tokio::pin!(document_failed);
        tokio::select! {
            biased;
            navigated = &mut error_page_committed => {
                record_commit(&self.page, &navigated);
                #[cfg(feature = "browser")]
                {
                    lock(&self.page.outcome).goto_unsettled = true;
                }
                Ok(())
            }
            () = &mut document_failed => {
                #[cfg(feature = "browser")]
                {
                    lock(&self.page.outcome).goto_unsettled = true;
                }
                Ok(())
            }
            () = stopped_after_a_drop => {
                #[cfg(feature = "browser")]
                {
                    lock(&self.page.outcome).goto_unsettled = true;
                }
                Ok(())
            }
            loaded = page.goto(url) => {
                if loaded.is_err() {
                    let _ = tokio::time::timeout(ACTION_SETTLE_LIMIT, async {
                        tokio::select! {
                            navigated = &mut error_page_committed => record_commit(&self.page, &navigated),
                            () = &mut document_failed => {}
                        }
                    }).await;
                }
                loaded.map(drop)
            },
        }
    }

    /// ~keep Whether a new navigation can start on the page at once. It cannot after
    /// ~keep [`Watch::goto`] ended on a frame's stop or an error-page commit: chromiumoxide then
    /// ~keep waits on the old navigation until its own 30 s deadline, and the page's next `goto`
    /// ~keep waits behind it.
    #[cfg(feature = "browser")]
    pub(crate) fn page_reusable(&self) -> bool {
        !lock(&self.page.outcome).goto_unsettled
    }

    pub(crate) fn mark_unsettled(&self) {
        lock(&self.page.outcome).goto_unsettled = true;
    }

    /// End the requested navigation: the navigations the page makes from now on are the
    /// caller's own, so the redirect limit no longer counts them.
    pub(crate) fn end_navigation(&self) {
        lock(&self.page.outcome).navigation_ended = true;
    }

    /// The status and headers of the main-frame document that `loader_id` committed, or `None`
    /// when no response was recorded for it (a `data:` URL, or Chrome's error page for a request
    /// that got no response).
    ///
    /// ~keep Keyed by the committed loader, not taken from the last response: Chrome commits
    /// ~keep no document for a 204 or a 2xx download, so the page keeps showing the previous one.
    pub(crate) fn document(&self, loader_id: &str) -> Option<DocumentResponse> {
        lock(&self.page.outcome).documents.get(loader_id).cloned()
    }

    /// The response paired with Chrome's main-frame commit or document load failure.
    pub(crate) fn committed_error_response(&self) -> Option<(String, DocumentResponse)> {
        failed_document_response(&lock(&self.page.outcome))
    }

    /// The first main-frame document request the check refused since the watch began.
    pub(crate) fn blocked_navigation(&self) -> Option<(String, String)> {
        lock(&self.page.outcome).blocked_navigation.clone()
    }

    /// Wait until every request of the page the check has taken is judged, and every request
    /// paused until now is matched to its page, for at most `ACTION_SETTLE_LIMIT`.
    pub(crate) async fn settle(&self) {
        self.settle_paused(None, Instant::now()).await;
    }

    /// Wait until every request of the page the check has taken is judged, and every request
    /// paused from `from` until `to` is matched to its page, for at most `ACTION_SETTLE_LIMIT`.
    ///
    /// ~keep A request is matched to its page before the page counts it, and matching a frame
    /// ~keep the registry does not know yet looks through the frame trees of every live target.
    /// ~keep A wait on the page's count alone ended during that match and missed the request (#192).
    async fn settle_paused(&self, from: Option<Instant>, to: Instant) -> usize {
        let deadline = Instant::now() + ACTION_SETTLE_LIMIT;
        let mut sleeps = 0;
        while self.unsettled(from, to) && Instant::now() < deadline {
            sleeps += 1;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        sleeps
    }

    /// Whether a request of the page is being judged, or a request paused from `from` until
    /// `to` is not matched to a page yet. A request paused outside that window cannot be the
    /// one the caller waits for, so waiting for it would only cost time.
    fn unsettled(&self, from: Option<Instant>, to: Instant) -> bool {
        self.page.in_flight.load(Ordering::Acquire) > 0
            || lock(&self.shared.unmatched)
                .iter()
                .any(|at| from.is_none_or(|from| *at >= from) && *at <= to)
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
        let _settle_sleeps = self.settle_paused(Some(started), cutoff).await;
        #[cfg(test)]
        lock(&self.shared.delays.action_waits).push((grace, _settle_sleeps));
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
        begin_ending(&self.page);
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
            begin_ending(&self.page);
            let _ = self.commands.send(Command::End {
                page: Arc::clone(&self.page),
                close_page: true,
                done: None,
            });
        }
    }
}

/// The entry of `refused`, a list [`Watch::refused_urls`] returned, that names `url`, or `None`
/// when the check did not refuse it. `url` is compared in the form the check records a refused
/// URL: parsed, without its userinfo or fragment, and credential-redacted.
pub(crate) fn listed_refusal(url: &str, refused: &[String]) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    userinfo::strip(&mut parsed);
    parsed.set_fragment(None);
    let listed = crate::net::redact_url_credentials(parsed.as_str());
    refused.contains(&listed).then_some(listed)
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

enum Lifecycle {
    Created(Arc<EventTargetCreated>),
    Destroyed(Arc<EventTargetDestroyed>),
}

enum ReconciledLifecycle {
    Created(TargetLookup),
    Destroyed(Arc<EventTargetDestroyed>, Option<bool>),
}

enum ChildPendingKind {
    Enable {
        done: oneshot::Sender<Result<(), String>>,
    },
    RecursiveArm {
        target: TargetId,
        session: String,
        waiting_for_debugger: bool,
        structural_parent: Option<String>,
    },
    StructuralArm {
        session: String,
    },
    Boundary,
    Close,
}

struct ChildPending {
    kind: ChildPendingKind,
    deadline: tokio::time::Instant,
}

struct RootWaiter {
    done: oneshot::Sender<Result<(), String>>,
    deadline: tokio::time::Instant,
}

struct StructuralTarget {
    target: TargetId,
    session: String,
    waiting_for_debugger: bool,
    authorized: bool,
    arm_acknowledged: bool,
    child_boundary_acknowledged: bool,
    deadline: tokio::time::Instant,
}

#[derive(serde::Deserialize)]
struct ChildAttached {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "targetInfo")]
    target_info: TargetInfo,
    #[serde(rename = "waitingForDebugger")]
    waiting_for_debugger: bool,
}

fn child_message(
    id: u64,
    method: &str,
    params: serde_json::Value,
    session: Option<&str>,
) -> async_tungstenite::tungstenite::Message {
    let mut command = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    if let Some(session) = session {
        command["sessionId"] = session.into();
    }
    async_tungstenite::tungstenite::Message::Text(command.to_string().into())
}

fn child_command_id(next_id: &mut u64) -> Option<u64> {
    let id = *next_id;
    *next_id = next_id.checked_add(1)?;
    Some(id)
}

fn child_pending(kind: ChildPendingKind, command_timeout: Duration) -> ChildPending {
    ChildPending {
        kind,
        deadline: tokio::time::Instant::now() + command_timeout,
    }
}

fn child_capacity_used(
    pending: &HashMap<u64, ChildPending>,
    root_waiters: &HashMap<TargetId, RootWaiter>,
    armed_targets: &HashSet<TargetId>,
    session_targets: &HashMap<String, TargetId>,
    structural_sessions: &HashMap<String, StructuralTarget>,
) -> usize {
    pending.len() + root_waiters.len() + armed_targets.len() + session_targets.len() + structural_sessions.len()
}

fn child_global_arm_params() -> serde_json::Value {
    serde_json::json!({
        "autoAttach": true,
        "waitForDebuggerOnStart": true,
        "flatten": true,
        "filter": [
            { "type": "browser", "exclude": true },
            { "type": "page", "exclude": true },
            {},
        ],
    })
}

fn child_recursive_arm_params() -> serde_json::Value {
    serde_json::json!({
        "autoAttach": true,
        "waitForDebuggerOnStart": true,
        "flatten": true,
        "filter": [
            { "type": "browser", "exclude": true },
            { "type": "tab", "exclude": true },
            {},
        ],
    })
}

fn take_ready_structural(
    structural_sessions: &mut HashMap<String, StructuralTarget>,
    session: &str,
) -> Option<StructuralTarget> {
    let ready = structural_sessions
        .get(session)
        .is_some_and(|target| target.authorized && target.arm_acknowledged && target.child_boundary_acknowledged);
    if ready {
        structural_sessions.remove(session)
    } else {
        None
    }
}

async fn child_socket_write<S>(
    socket: &mut async_tungstenite::WebSocketStream<S>,
    message: async_tungstenite::tungstenite::Message,
    command_timeout: Duration,
) -> Result<(), String>
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(command_timeout, socket.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("failed to write child-target controller command: {error}")),
        Err(_) => Err("timed out writing child-target controller command".to_owned()),
    }
}

async fn close_child_socket<S>(socket: &mut async_tungstenite::WebSocketStream<S>, command_timeout: Duration) -> bool
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(command_timeout, socket.close(None))
        .await
        .is_ok_and(|closed| closed.is_ok())
}

async fn controller_completed<F>(controller: F) -> bool
where
    F: std::future::Future<Output = bool>,
{
    std::panic::AssertUnwindSafe(controller)
        .catch_unwind()
        .await
        .unwrap_or(false)
}

fn child_policy_acknowledged(registry: &Registry, target: &TargetId) -> bool {
    matches!(
        registry.ownership.get(target),
        Some(TargetOwnership::Watched(page)) if !page.ending.load(Ordering::Acquire)
    )
}

fn child_policy_boundary_exists(registry: &Registry, target: &TargetId) -> bool {
    match registry.ownership.get(target) {
        Some(TargetOwnership::Watched(page)) => !page.ending.load(Ordering::Acquire),
        Some(TargetOwnership::PendingContext(context)) => registry.contexts.contains_key(context),
        Some(TargetOwnership::PendingShared) => registry.shared_context,
        Some(TargetOwnership::Other | TargetOwnership::Quarantined) | None => false,
    }
}

/// Hold every related target at `waitForDebugger` until its authoritative context has selected
/// an immutable policy and any competing chromiumoxide page auto-attach has been disabled. ~keep
async fn serve_child_targets(
    socket: async_tungstenite::WebSocketStream<async_tungstenite::tokio::ConnectStream>,
    mut commands: mpsc::Receiver<ChildCommand>,
    shared: &Arc<Shared>,
) -> bool {
    serve_child_targets_with_timeout(socket, &mut commands, shared, CHILD_COMMAND_TIMEOUT).await
}

async fn serve_child_targets_with_timeout<S>(
    mut socket: async_tungstenite::WebSocketStream<S>,
    commands: &mut mpsc::Receiver<ChildCommand>,
    shared: &Arc<Shared>,
    command_timeout: Duration,
) -> bool
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin,
{
    let mut next_id = 1_u64;
    let mut pending = HashMap::<u64, ChildPending>::new();
    let mut root_waiters = HashMap::<TargetId, RootWaiter>::new();
    let mut armed_targets = HashSet::<TargetId>::new();
    let mut session_targets = HashMap::<String, TargetId>::new();
    let mut structural_sessions = HashMap::<String, StructuralTarget>::new();
    let mut global_requested = false;
    let mut global_armed = false;
    let tick = command_timeout.min(Duration::from_millis(10));
    let mut deadlines = tokio::time::interval(tick.max(Duration::from_millis(1)));
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(ChildCommand::Enable { done }) => {
                    if global_armed {
                        let _ = done.send(Ok(()));
                        continue;
                    }
                    if child_capacity_used(
                        &pending,
                        &root_waiters,
                        &armed_targets,
                        &session_targets,
                        &structural_sessions,
                    )
                        >= CHILD_CONTROLLER_PENDING_LIMIT
                    {
                        let _ = done.send(Err("child-target controller capacity exceeded".to_owned()));
                        return false;
                    }
                    let Some(id) = child_command_id(&mut next_id) else {
                        let _ = done.send(Err("child-target controller command id exhausted".to_owned()));
                        return false;
                    };
                    if let Err(error) = child_socket_write(
                        &mut socket,
                        child_message(id, "Target.setAutoAttach", child_global_arm_params(), None),
                        command_timeout,
                    )
                    .await
                    {
                        let _ = done.send(Err(format!(
                            "failed to arm browser-wide child-target interception: {error}"
                        )));
                        return false;
                    }
                    global_requested = true;
                    pending.insert(
                        id,
                        child_pending(ChildPendingKind::Enable { done }, command_timeout),
                    );
                }
                Some(ChildCommand::Arm { root, done }) => {
                    if !global_armed {
                        let _ = done.send(Err("browser-wide child-target interception is not enabled".to_owned()));
                        return false;
                    }
                    if !child_policy_acknowledged(&lock(&shared.registry), &root) {
                        let _ = done.send(Err("root target has no acknowledged policy".to_owned()));
                        return false;
                    }
                    if armed_targets.contains(&root) {
                        let _ = done.send(Ok(()));
                        continue;
                    }
                    if child_capacity_used(
                        &pending,
                        &root_waiters,
                        &armed_targets,
                        &session_targets,
                        &structural_sessions,
                    )
                        >= CHILD_CONTROLLER_PENDING_LIMIT
                    {
                        let _ = done.send(Err("child-target controller capacity exceeded".to_owned()));
                        return false;
                    }
                    if let Some(previous) = root_waiters.insert(
                        root,
                        RootWaiter {
                            done,
                            deadline: tokio::time::Instant::now() + command_timeout,
                        },
                    ) {
                        let _ = previous.done.send(Err("root target arm was superseded".to_owned()));
                        return false;
                    }
                }
                Some(ChildCommand::Stop) | None => {
                    return close_child_socket(&mut socket, command_timeout).await;
                }
            },
            _ = deadlines.tick() => {
                let now = tokio::time::Instant::now();
                root_waiters.retain(|_, waiter| !waiter.done.is_closed());
                if let Some(root) = root_waiters
                    .iter()
                    .find_map(|(root, waiter)| (waiter.deadline <= now).then(|| root.clone()))
                {
                    if let Some(waiter) = root_waiters.remove(&root) {
                        let _ = waiter
                            .done
                            .send(Err("timed out waiting for the root target attachment".to_owned()));
                    }
                    return false;
                }
                if structural_sessions.values().any(|target| target.deadline <= now) {
                    return false;
                }
                let expired = pending
                    .iter()
                    .find_map(|(id, command)| (command.deadline <= now).then_some(*id));
                if let Some(id) = expired {
                    if let Some(command) = pending.remove(&id) {
                        match command.kind {
                            ChildPendingKind::Enable { done } => {
                                let _ = done.send(Err(
                                    "timed out waiting for browser-wide child-target controller acknowledgement"
                                        .to_owned(),
                                ));
                            }
                            ChildPendingKind::RecursiveArm { target, .. } => {
                                if let Some(waiter) = root_waiters.remove(&target) {
                                    let _ = waiter.done.send(Err(
                                        "timed out waiting for child-target controller acknowledgement".to_owned(),
                                    ));
                                }
                            }
                            ChildPendingKind::StructuralArm { .. } => {}
                            ChildPendingKind::Boundary | ChildPendingKind::Close => {}
                        }
                    }
                    return false;
                }
            }
            message = socket.next() => {
                let Some(Ok(message)) = message else { return false };
                let text = match message {
                    async_tungstenite::tungstenite::Message::Text(text) => text,
                    async_tungstenite::tungstenite::Message::Ping(payload) => {
                        if child_socket_write(
                            &mut socket,
                            async_tungstenite::tungstenite::Message::Pong(payload),
                            command_timeout,
                        ).await.is_err() {
                            return false;
                        }
                        continue;
                    }
                    async_tungstenite::tungstenite::Message::Close(_) => return false,
                    _ => continue,
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(text.as_ref()) else {
                    return false;
                };
                if let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) {
                    let Some(command) = pending.remove(&id) else { continue };
                    let error = value.get("error").map(serde_json::Value::to_string);
                    let acknowledged = error.is_none() && value.get("result").is_some();
                    match command.kind {
                        ChildPendingKind::Enable { done } => {
                            if !acknowledged {
                                let _ = done.send(Err(format!(
                                    "failed to arm browser-wide child-target interception: {}",
                                    error.as_deref().unwrap_or("missing CDP result")
                                )));
                                return false;
                            }
                            global_armed = true;
                            let _ = done.send(Ok(()));
                        }
                        ChildPendingKind::RecursiveArm {
                            target,
                            session,
                            waiting_for_debugger,
                            structural_parent,
                        } => {
                            if !acknowledged {
                                if let Some(waiter) = root_waiters.remove(&target) {
                                    let _ = waiter.done.send(Err(format!(
                                        "failed to arm child-target interception for {}: {}",
                                        target.inner(),
                                        error.as_deref().unwrap_or("missing CDP result")
                                    )));
                                }
                                return false;
                            }
                            if armed_targets.len() >= CHILD_CONTROLLER_PENDING_LIMIT
                                && !armed_targets.contains(&target)
                            {
                                return false;
                            }
                            armed_targets.insert(target.clone());
                            // ~keep Chrome reports a page under a debugger-held tab as not waiting,
                            // ~keep but resuming only the tab leaves window.open blocked. Resume the
                            // ~keep child session after its recursive interception boundary is armed.
                            if waiting_for_debugger || structural_parent.is_some() {
                                let Some(id) = child_command_id(&mut next_id) else { return false };
                                if let Err(write_error) = child_socket_write(&mut socket, child_message(
                                    id,
                                    "Runtime.runIfWaitingForDebugger",
                                    serde_json::json!({}),
                                    Some(&session),
                                ), command_timeout).await {
                                    if let Some(waiter) = root_waiters.remove(&target) {
                                        let _ = waiter.done.send(Err(format!(
                                            "failed to resume the armed root target: {write_error}"
                                        )));
                                    }
                                    return false;
                                }
                                pending.insert(id, child_pending(ChildPendingKind::Boundary, command_timeout));
                            }
                            let ready_parent = if let Some(parent_session) = structural_parent {
                                let Some(parent) = structural_sessions.get_mut(&parent_session) else {
                                    return false;
                                };
                                parent.child_boundary_acknowledged = true;
                                take_ready_structural(&mut structural_sessions, &parent_session)
                            } else {
                                None
                            };
                            if let Some(parent) = ready_parent
                                && parent.waiting_for_debugger
                            {
                                let Some(id) = child_command_id(&mut next_id) else { return false };
                                if child_socket_write(
                                    &mut socket,
                                    child_message(
                                        id,
                                        "Runtime.runIfWaitingForDebugger",
                                        serde_json::json!({}),
                                        Some(&parent.session),
                                    ),
                                    command_timeout,
                                )
                                .await
                                .is_err()
                                {
                                    return false;
                                }
                                pending.insert(id, child_pending(ChildPendingKind::Boundary, command_timeout));
                            }
                            if let Some(waiter) = root_waiters.remove(&target) {
                                if !child_policy_acknowledged(&lock(&shared.registry), &target) {
                                    let _ = waiter.done.send(Err("root target has no acknowledged policy".to_owned()));
                                    return false;
                                }
                                let _ = waiter.done.send(Ok(()));
                            }
                        }
                        ChildPendingKind::StructuralArm { session } => {
                            if !acknowledged {
                                return false;
                            }
                            let Some(target) = structural_sessions.get_mut(&session) else {
                                return false;
                            };
                            target.arm_acknowledged = true;
                            let ready = take_ready_structural(&mut structural_sessions, &session);
                            if let Some(target) = ready
                                && target.waiting_for_debugger
                            {
                                let Some(id) = child_command_id(&mut next_id) else { return false };
                                if child_socket_write(
                                    &mut socket,
                                    child_message(
                                        id,
                                        "Runtime.runIfWaitingForDebugger",
                                        serde_json::json!({}),
                                        Some(&target.session),
                                    ),
                                    command_timeout,
                                )
                                .await
                                .is_err()
                                {
                                    return false;
                                }
                                pending.insert(id, child_pending(ChildPendingKind::Boundary, command_timeout));
                            }
                        }
                        ChildPendingKind::Boundary | ChildPendingKind::Close if !acknowledged => return false,
                        ChildPendingKind::Boundary | ChildPendingKind::Close => {}
                    }
                    continue;
                }
                match value.get("method").and_then(serde_json::Value::as_str) {
                    Some("Target.targetDestroyed") => {
                        let Some(target) = value["params"]["targetId"].as_str().map(TargetId::new) else {
                            return false;
                        };
                        if structural_sessions.values().any(|structural| structural.target == target) {
                            return false;
                        }
                        armed_targets.remove(&target);
                        session_targets.retain(|_, attached| attached != &target);
                        if let Some(waiter) = root_waiters.remove(&target) {
                            let _ = waiter.done.send(Err("root target was destroyed before it was armed".to_owned()));
                            return false;
                        }
                        continue;
                    }
                    Some("Target.detachedFromTarget") => {
                        let Some(session) = value["params"]["sessionId"].as_str() else {
                            return false;
                        };
                        if structural_sessions.remove(session).is_some() {
                            return false;
                        }
                        if let Some(target) = session_targets.remove(session) {
                            armed_targets.remove(&target);
                            if let Some(waiter) = root_waiters.remove(&target) {
                                let _ = waiter.done.send(Err(
                                    "root target detached before its boundary was acknowledged".to_owned(),
                                ));
                                return false;
                            }
                        }
                        continue;
                    }
                    Some("Target.attachedToTarget") => {}
                    _ => continue,
                }
                let Ok(attached) = serde_json::from_value::<ChildAttached>(value["params"].clone()) else {
                    return false;
                };
                let parent_session = value
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if !global_requested {
                    return false;
                }
                if child_capacity_used(
                    &pending,
                    &root_waiters,
                    &armed_targets,
                    &session_targets,
                    &structural_sessions,
                )
                    >= CHILD_CONTROLLER_PENDING_LIMIT
                {
                    return false;
                }
                if attached.target_info.r#type == "tab" {
                    let _ = adopt_target(shared, &attached.target_info);
                    let authorized = {
                        let registry = lock(&shared.registry);
                        child_policy_boundary_exists(&registry, &attached.target_info.target_id)
                            || matches!(
                                registry.ownership.get(&attached.target_info.target_id),
                                Some(TargetOwnership::Other)
                            )
                    };
                    let structural = StructuralTarget {
                        target: attached.target_info.target_id,
                        session: attached.session_id.clone(),
                        waiting_for_debugger: attached.waiting_for_debugger,
                        authorized,
                        arm_acknowledged: false,
                        child_boundary_acknowledged: false,
                        deadline: tokio::time::Instant::now() + command_timeout,
                    };
                    if structural_sessions
                        .insert(attached.session_id.clone(), structural)
                        .is_some()
                    {
                        return false;
                    }
                    let Some(id) = child_command_id(&mut next_id) else { return false };
                    if child_socket_write(
                        &mut socket,
                        child_message(
                            id,
                            "Target.setAutoAttach",
                            child_recursive_arm_params(),
                            Some(&attached.session_id),
                        ),
                        command_timeout,
                    )
                    .await
                    .is_err()
                    {
                        structural_sessions.remove(&attached.session_id);
                        return false;
                    }
                    pending.insert(
                        id,
                        child_pending(
                            ChildPendingKind::StructuralArm {
                                session: attached.session_id,
                            },
                            command_timeout,
                        ),
                    );
                    continue;
                }
                let _ = adopt_target(shared, &attached.target_info);
                let target = attached.target_info.target_id.clone();
                let (permitted, other) = {
                    let registry = lock(&shared.registry);
                    (
                        child_policy_boundary_exists(&registry, &target),
                        matches!(registry.ownership.get(&target), Some(TargetOwnership::Other)),
                    )
                };
                let structural_parent = parent_session.filter(|session| structural_sessions.contains_key(session));
                if !(attached.waiting_for_debugger || structural_parent.is_some() || permitted || other) {
                    return false;
                }
                if !(permitted || other) {
                    if attached.target_info.r#type != "iframe" {
                        if lock(&shared.registry).opened.contains_key(&target) {
                            return false;
                        }
                        // ~keep Closing a page under a paused tab destroys that tab too. Retire
                        // ~keep its pending boundary before the expected detach and late arm ACK.
                        if let Some(parent) = &structural_parent {
                            structural_sessions.remove(parent);
                            pending.retain(|_, command| {
                                !matches!(&command.kind, ChildPendingKind::StructuralArm { session } if session == parent)
                            });
                        }
                        let Some(id) = child_command_id(&mut next_id) else { return false };
                        if child_socket_write(&mut socket, child_message(
                                    id,
                                    "Target.closeTarget",
                                    serde_json::json!({ "targetId": target }),
                                    None,
                                ), command_timeout).await.is_err() {
                            return false;
                        }
                        pending.insert(id, child_pending(ChildPendingKind::Close, command_timeout));
                    }
                    continue;
                }
                if let Some(parent) = structural_parent
                    .as_deref()
                    .and_then(|session| structural_sessions.get_mut(session))
                {
                    parent.authorized = true;
                }
                let Some(id) = child_command_id(&mut next_id) else { return false };
                if child_socket_write(&mut socket, child_message(
                    id,
                    "Target.setAutoAttach",
                    child_recursive_arm_params(),
                    Some(&attached.session_id),
                ), command_timeout).await.is_err() {
                    return false;
                }
                session_targets.insert(attached.session_id.clone(), target.clone());
                pending.insert(
                    id,
                    child_pending(
                        ChildPendingKind::RecursiveArm {
                            target,
                            session: attached.session_id,
                            waiting_for_debugger: attached.waiting_for_debugger,
                            structural_parent,
                        },
                        command_timeout,
                    ),
                );
            }
        }
    }
}

enum TargetLookup {
    Live(Box<TargetInfo>),
    Absent,
    Failed,
}

/// The listener. It runs the watch commands in order, tracks the targets each watched page owns
/// and the pages the check opened, and answers the paused requests concurrently, so a slow DNS
/// lookup for one page does not hold up the others. Interception is turned off only on a stop,
/// once every page of the check is dropped and every answer and every watch end already
/// started has finished, and on an external browser once every target of a closed page is
/// destroyed. It is never turned off when the pages share the browser's own context, when the
/// browser is killed. An external browser is disabled only after its owned contexts and
/// targets are authoritatively absent and the pending refusals have been delivered. ~keep
async fn serve(
    browser: Arc<Browser>,
    shared: Arc<Shared>,
    mut events: Events,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> bool {
    let browser = &*browser;
    let shared = &*shared;
    let mut running: FuturesUnordered<BoxFuture<'_, Done>> = FuturesUnordered::new();
    // ~keep What a stop waits for: the tasks running when it arrived. Requests paused after it
    // ~keep are still answered, but not waited for, so a page that keeps sending cannot hold
    // ~keep the stop off.
    let mut draining: FuturesUnordered<BoxFuture<'_, Done>> = FuturesUnordered::new();
    let mut stopping: Option<Vec<oneshot::Sender<()>>> = None;
    let mut commands_open = true;
    let mut navigations: SelectAll<BoxStream<'static, Committed>> = SelectAll::new();
    let mut failures: SelectAll<BoxStream<'static, Failed>> = SelectAll::new();
    // ~keep Queries run concurrently so CDP latency cannot stall requests or commands, while
    // ~keep `FuturesOrdered` applies their answers in the lifecycle-event order consumed here.
    let mut lifecycles: FuturesOrdered<BoxFuture<'_, ReconciledLifecycle>> = FuturesOrdered::new();
    let mut teardown_checks = tokio::time::interval(Duration::from_millis(25));
    let mut inspect_teardown = false;
    loop {
        if draining.is_empty()
            && inspect_teardown
            && let Some(stopped) = stopping.take()
        {
            inspect_teardown = false;
            if shared.context != PageContext::Shared
                && shared.origin != BrowserOrigin::Killed
                && !owned_targets_absent(browser, shared).await
            {
                stopping = Some(stopped);
                continue;
            }
            // ~keep Only the completed context-disposal census makes disabling safe: Chrome
            // ~keep otherwise releases requests from a watched page that has not died yet.
            if shared.context != PageContext::Shared
                && shared.origin != BrowserOrigin::Killed
                && !tokio::time::timeout(
                    CONTROLLER_TEARDOWN_TIMEOUT,
                    browser.execute(FetchDisableParams::default()),
                )
                .await
                .is_ok_and(|disabled| disabled.is_ok())
            {
                return false;
            }
            for done in stopped {
                let _ = done.send(());
            }
            return true;
        }
        tokio::select! {
            command = commands.recv(), if commands_open => match command {
                // ~keep A page opened while the check stops is dropped at once: its watch is
                // ~keep refused below, and a page left behind would outlive the check. The drop
                // ~keep joins the drain, since the loop ends, and turns interception off, as
                // ~keep soon as the drain is empty, whatever is still running.
                Some(Command::Opened(root, context)) if stopping.is_some() => {
                    if let Some(context) = &context {
                        lock(&shared.registry)
                            .context_targets
                            .insert(root.clone(), context.clone());
                    }
                    draining.push(Box::pin(async move {
                        #[cfg(test)]
                        lock(&shared.delays.dropped).push(root.clone());
                        drop_page(browser, shared, root, context).await;
                        Done::Dropped
                    }));
                }
                Some(Command::Opened(root, context)) => {
                    let mut registry = lock(&shared.registry);
                    if let Some(context) = &context {
                        registry.context_targets.insert(root.clone(), context.clone());
                    }
                    registry.opened.insert(root, context);
                }
                Some(Command::Abandon(root)) => {
                    let context = {
                        let mut registry = lock(&shared.registry);
                        if let Some(owner) = registry.owner_of_target(root.inner()) {
                            begin_ending(&owner);
                        }
                        registry.opened.remove(&root)
                    };
                    if let Some(context) = context {
                        let dropping = Box::pin(async move {
                            drop_page(browser, shared, root, context).await;
                            Done::Dropped
                        });
                        if stopping.is_some() {
                            draining.push(dropping);
                        } else {
                            running.push(dropping);
                        }
                    }
                }
                Some(Command::Watch(_, _, _, ack)) if stopping.is_some() => {
                    let _ = ack.send(Err("request interception stopped".to_owned()));
                }
                Some(Command::Watch(page, navigated, failed, ack)) => {
                    let mut registry = lock(&shared.registry);
                    if !registry.opened.contains_key(&page.root) {
                        drop(registry);
                        let _ = ack.send(Err(
                            "the SSRF check can only watch a page it opened; open the page with the check".to_owned(),
                        ));
                        continue;
                    }
                    if let Err(error) = install_watch(&mut registry, &page) {
                        drop(registry);
                        let _ = ack.send(Err(error));
                        continue;
                    }
                    drop(registry);
                    navigations.push(commits_of(&page, navigated));
                    failures.push(failures_of(&page, failed));
                    let _ = ack.send(Ok(()));
                }
                Some(Command::End { page, close_page, done }) => {
                    running.push(Box::pin(end_watch(browser, shared, page, close_page, done)));
                }
                Some(Command::Stop(done)) => {
                    let stopped = stopping.get_or_insert_with(|| {
                        lifecycles.clear();
                        draining.extend(std::mem::take(&mut running));
                        let left = std::mem::take(&mut lock(&shared.registry).opened);
                        for (root, context) in left {
                            draining.push(Box::pin(async move {
                                #[cfg(test)]
                                lock(&shared.delays.dropped).push(root.clone());
                                drop_page(browser, shared, root, context).await;
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
                    let paused = Paused::new(shared, Instant::now());
                    running.push(Box::pin(async move {
                        answer(browser, shared, &event, paused).await;
                        Done::Answered
                    }));
                }
                None => return false,
            },
            event = events.created.next(), if stopping.is_none() => {
                let Some(event) = event else { return false };
                lifecycles.push_back(reconcile_lifecycle(browser, shared, Lifecycle::Created(event)));
            }
            event = events.destroyed.next(), if stopping.is_none() => {
                let Some(event) = event else { return false };
                lifecycles.push_back(reconcile_lifecycle(browser, shared, Lifecycle::Destroyed(event)));
            }
            Some(event) = lifecycles.next(), if !lifecycles.is_empty() && stopping.is_none() => {
                if !apply_lifecycle(browser, shared, event, &mut running) {
                    return false;
                }
            }
            Some((page, failed)) = failures.next(), if !failures.is_empty() => {
                record_document_failure(&page, &failed);
            }
            Some(done) = running.next(), if !running.is_empty() => settle(shared, done),
            Some(done) = draining.next(), if !draining.is_empty() => settle(shared, done),
            _ = teardown_checks.tick(), if draining.is_empty() && stopping.is_some() => {
                inspect_teardown = true;
            }
        }
    }
}

async fn owned_targets_absent(browser: &Browser, shared: &Shared) -> bool {
    if shared.opening.load(Ordering::Acquire) != 0 {
        return false;
    }
    let Ok(contexts) = browser.execute(GetBrowserContextsParams::default()).await else {
        return false;
    };
    let Ok(targets) = browser.execute(GetTargetsParams::default()).await else {
        return false;
    };
    let owned_contexts: HashSet<_> = lock(&shared.registry).contexts.keys().cloned().collect();
    if contexts
        .result
        .browser_context_ids
        .iter()
        .any(|context| owned_contexts.contains(context))
    {
        return false;
    }
    let absent = {
        let registry = lock(&shared.registry);
        !targets.result.target_infos.iter().any(|info| {
            info.browser_context_id
                .as_ref()
                .is_some_and(|context| owned_contexts.contains(context))
                || registry
                    .ownership
                    .get(&info.target_id)
                    .is_some_and(|ownership| !matches!(ownership, TargetOwnership::Other))
        })
    };
    if absent {
        let mut registry = lock(&shared.registry);
        for context in &owned_contexts {
            let _ = registry.retire_context(context);
        }
    }
    absent
}

fn reconcile_lifecycle<'a>(
    browser: &'a Browser,
    shared: &'a Shared,
    event: Lifecycle,
) -> BoxFuture<'a, ReconciledLifecycle> {
    async move {
        match event {
            Lifecycle::Created(event) => {
                let info = live_target_info(browser, shared, &event.target_info.target_id).await;
                ReconciledLifecycle::Created(info)
            }
            Lifecycle::Destroyed(event) => {
                let live = target_is_live(browser, shared, &event.target_id).await;
                ReconciledLifecycle::Destroyed(event, live)
            }
        }
    }
    .boxed()
}

fn apply_lifecycle<'a>(
    browser: &'a Browser,
    shared: &'a Shared,
    event: ReconciledLifecycle,
    running: &mut FuturesUnordered<BoxFuture<'a, Done>>,
) -> bool {
    match event {
        ReconciledLifecycle::Created(TargetLookup::Live(info)) => {
            if let Some(close) = reconcile_created(shared, Some(&info)) {
                running.push(Box::pin(async move {
                    let _ = browser.execute(CloseTargetParams::new(close)).await;
                    Done::Closed
                }));
            }
        }
        ReconciledLifecycle::Destroyed(event, Some(live)) => {
            let mut registry = lock(&shared.registry);
            if reconcile_destroyed(&mut registry, &event.target_id, live) {
                let context = registry.opened.remove(&event.target_id).flatten();
                drop(registry);
                shared.destroyed.notify_waiters();
                // ~keep A parked page the session pool evicts, or a pooled page dropped before
                // ~keep its watch, is closed by its owner, not through a watch; its context
                // ~keep goes here, and its popups with it.
                if let Some(context) = context {
                    running.push(Box::pin(async move {
                        let _ = dispose_and_retire_context(browser, shared, context).await;
                        Done::Dropped
                    }));
                }
            }
        }
        ReconciledLifecycle::Created(TargetLookup::Failed) => return false,
        ReconciledLifecycle::Created(TargetLookup::Absent) | ReconciledLifecycle::Destroyed(_, None) => {}
    }
    true
}

/// The live target record is the authoritative source of its browser context. Creation events
/// may omit that field, so ownership is never installed from their lineage alone. ~keep
async fn live_target_info(browser: &Browser, _shared: &Shared, target: &TargetId) -> TargetLookup {
    #[cfg(test)]
    wait_for_lifecycle_query(_shared, target).await;
    let Ok(response) = browser.execute(GetTargetsParams::default()).await else {
        return TargetLookup::Failed;
    };
    response
        .result
        .target_infos
        .into_iter()
        .find(|info| info.target_id == *target)
        .map_or(TargetLookup::Absent, |info| TargetLookup::Live(Box::new(info)))
}

/// Whether Chrome currently lists `target`; `None` leaves ownership unchanged and fail-closed.
/// ~keep Chrome can change after this lock-free query: the later queued lifecycle event then
/// ~keep applies the converging mutation (destroy removes a created ghost; create restores reuse).
async fn target_is_live(browser: &Browser, _shared: &Shared, target: &TargetId) -> Option<bool> {
    #[cfg(test)]
    wait_for_lifecycle_query(_shared, target).await;
    browser.execute(GetTargetsParams::default()).await.ok().map(|response| {
        response
            .result
            .target_infos
            .iter()
            .any(|info| info.target_id == *target)
    })
}

#[cfg(test)]
async fn wait_for_lifecycle_query(shared: &Shared, target: &TargetId) {
    if let Some(gate) = &shared.delays.lifecycle_query_gate {
        lock(&gate.held).push(target.inner().clone());
        if let Ok(permit) = gate.permits.acquire().await {
            permit.forget();
        }
    }
}

fn reconcile_created(shared: &Shared, info: Option<&TargetInfo>) -> Option<TargetId> {
    info.and_then(|info| adopt_target(shared, info))
}

fn reconcile_destroyed(registry: &mut Registry, target: &TargetId, live: bool) -> bool {
    if live {
        return false;
    }
    registry.remove_target(target);
    true
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

fn install_watch(registry: &mut Registry, page: &Arc<WatchedPage>) -> Result<(), String> {
    let context = registry.opened.get(&page.root).cloned().flatten();
    if let Some(context) = &context {
        let expected = PolicyIdentity::from(&page.config.ssrf);
        let registered = registry
            .contexts
            .get(context)
            .ok_or_else(|| "the page's browser context has no registered policy".to_owned())?;
        if registered.policy.as_ref().is_some_and(|policy| policy != &expected) {
            return Err("the page's browser context was opened under a different SSRF policy".to_owned());
        }
    }
    registry.pages.push(Arc::clone(page));
    // ~keep Replace ownership under the registry lock: a popup event processed before this
    // ~keep remains fail-closed under the ending watch, and one processed after uses this policy.
    registry.register_watched(page.root.clone(), Arc::clone(page));
    if let Some(context) = context {
        let pending: Vec<_> = registry
            .ownership
            .iter()
            .filter(|(_, ownership)| {
                matches!(ownership, TargetOwnership::PendingContext(pending_context) if *pending_context == context)
            })
            .map(|(target, _)| target.clone())
            .collect();
        if let Some(registered) = registry.contexts.get_mut(&context) {
            registered.owner = Some(Arc::clone(page));
        }
        for target in pending {
            registry.register_watched(target, Arc::clone(page));
        }
    } else if registry.shared_context {
        registry.default_context_owner = Some(Arc::clone(page));
        let pending: Vec<_> = registry
            .ownership
            .iter()
            .filter(|(_, ownership)| matches!(ownership, TargetOwnership::PendingShared))
            .map(|(target, _)| target.clone())
            .collect();
        for target in pending {
            registry.register_watched(target, Arc::clone(page));
        }
    }
    Ok(())
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
fn adopt_target(shared: &Shared, info: &TargetInfo) -> Option<TargetId> {
    let mut registry = lock(&shared.registry);
    if matches!(info.r#type.as_str(), "tab" | "iframe") {
        registry.structural_targets.insert(info.target_id.clone());
    }
    let (lineage_owner, frame_owner) = match (&info.opener_id, &info.parent_frame_id) {
        (Some(opener), _) => (registry.owner_of_target(opener.inner()), None),
        (None, Some(parent)) => {
            let frame_owner = registry.frame_owner(parent);
            let owner = frame_owner.as_ref().and_then(|resolved| resolved.owner.clone());
            (owner, frame_owner)
        }
        (None, None) => (None, None),
    };
    let context = info.browser_context_id.as_ref();
    let context_was_retired = context.is_some_and(|context| {
        registry.retired_contexts.contains(context)
            || (registry.context_targets.get(&info.target_id) == Some(context)
                && !registry.contexts.contains_key(context))
    });
    if context_was_retired {
        registry.register_quarantined(info.target_id.clone(), context.cloned());
        return (info.r#type != "iframe").then(|| info.target_id.clone());
    }
    let owned_context = context.and_then(|id| registry.contexts.get(id).map(|registered| (id, registered)));
    let context_owner = owned_context
        .and_then(|(_, registered)| registered.owner.clone())
        .or_else(|| {
            (context.is_none() && registry.shared_context)
                .then(|| registry.default_context_owner.clone())
                .flatten()
        });
    let context_is_owned = owned_context.is_some() || (context.is_none() && registry.shared_context);
    if let Some((context, _)) = owned_context {
        registry.context_targets.insert(info.target_id.clone(), context.clone());
    }
    let owner = match (context_owner, lineage_owner) {
        (Some(context_owner), Some(lineage_owner)) if !Arc::ptr_eq(&context_owner, &lineage_owner) => {
            registry.register_quarantined(info.target_id.clone(), context.cloned());
            return (info.r#type != "iframe").then(|| info.target_id.clone());
        }
        (Some(context_owner), _) => Some(context_owner),
        (None, _) if context_is_owned => {
            if let Some(context) = context {
                registry.register_pending(info.target_id.clone(), context.clone());
            } else {
                registry.register_pending_shared(info.target_id.clone());
            }
            return None;
        }
        (None, Some(_)) => {
            registry.register_quarantined(info.target_id.clone(), context.cloned());
            return (info.r#type != "iframe").then(|| info.target_id.clone());
        }
        (None, None) => None,
    };
    let Some(owner) = owner else {
        registry.register_other(info.target_id.clone());
        return None;
    };
    if info.r#type == "iframe" || frame_owner.is_some() {
        registry
            .ownership
            .insert(info.target_id.clone(), TargetOwnership::Watched(Arc::clone(&owner)));
        registry.target_generations.insert(info.target_id.clone(), Arc::new(()));
        if let Some(frame_owner) = frame_owner {
            registry
                .frames
                .insert(FrameId::new(info.target_id.inner()), frame_owner);
        }
        return None;
    }
    let ending = owner.ending.load(Ordering::Acquire);
    registry.register_watched(info.target_id.clone(), owner);
    (ending && !registry.structural_targets.contains(&info.target_id)).then(|| info.target_id.clone())
}

/// A main-frame commit of a watched page: the page and the navigation event.
type Committed = (Arc<WatchedPage>, Arc<EventFrameNavigated>);
type Failed = (Arc<WatchedPage>, Arc<EventLoadingFailed>);

/// The navigation events of `page`'s own target, until its watch ends.
fn commits_of(
    page: &Arc<WatchedPage>,
    navigated: chromiumoxide::listeners::EventStream<EventFrameNavigated>,
) -> BoxStream<'static, Committed> {
    events_of(page, navigated)
}

fn failures_of(
    page: &Arc<WatchedPage>,
    failed: chromiumoxide::listeners::EventStream<EventLoadingFailed>,
) -> BoxStream<'static, Failed> {
    events_of(page, failed)
}

fn events_of<T, S>(page: &Arc<WatchedPage>, events: S) -> BoxStream<'static, (Arc<WatchedPage>, Arc<T>)>
where
    T: Send + Sync + 'static,
    S: Stream<Item = Arc<T>> + Send + 'static,
{
    let page = Arc::clone(page);
    events
        .take_until(events_ended(Arc::clone(&page)))
        .map(move |event| (Arc::clone(&page), event))
        .boxed()
}

async fn events_ended(page: Arc<WatchedPage>) {
    loop {
        let notified = page.events_ended.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if page.ending.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

fn begin_ending(page: &WatchedPage) {
    page.ending.store(true, Ordering::Release);
    page.events_ended.notify_waiters();
}

/// Record the loader of a document `page`'s main frame committed.
fn record_commit(page: &WatchedPage, navigated: &EventFrameNavigated) {
    if navigated.frame.id == page.main_frame {
        record_main_frame_commit(
            &page.outcome,
            navigated.frame.loader_id.as_ref(),
            navigated.frame.unreachable_url.as_deref(),
        );
    }
}

fn record_main_frame_commit(state: &Mutex<InterceptOutcome>, loader_id: &str, unreachable_url: Option<&str>) {
    let mut state = lock(state);
    state.committed_loader = Some(loader_id.to_owned());
    state.committed_unreachable_url = unreachable_url.map(str::to_owned);
    if unreachable_url.is_none() {
        state.failed_document = None;
    }
}

fn failed_document_response(state: &InterceptOutcome) -> Option<(String, DocumentResponse)> {
    if let (Some(failed_url), Some(loader_id)) = (&state.committed_unreachable_url, &state.committed_loader)
        && let Some(response) = state.documents.get(loader_id)
    {
        return Some((failed_url.clone(), response.clone()));
    }
    let request_id = state.failed_document.as_deref()?;
    let response = state.documents.get(request_id)?.clone();
    Some((response.url.clone(), response))
}

fn record_document_failure(page: &WatchedPage, event: &EventLoadingFailed) {
    if event.r#type != ResourceType::Document {
        return;
    }
    let mut outcome = lock(&page.outcome);
    if record_document_failure_outcome(
        &mut outcome,
        event.request_id.as_ref(),
        event.canceled == Some(true),
        &event.error_text,
    ) {
        drop(outcome);
        page.document_failed.notify_waiters();
    }
}

fn record_document_failure_outcome(
    state: &mut InterceptOutcome,
    request_id: &str,
    canceled: bool,
    error_text: &str,
) -> bool {
    if canceled || error_text.eq_ignore_ascii_case("net::ERR_ABORTED") {
        return false;
    }
    if state.latest_document_request.as_deref() != Some(request_id) {
        return false;
    }
    if !state.documents.get(request_id).is_some_and(|response| {
        (200..300).contains(&response.status) && !NO_DOCUMENT_STATUSES.contains(&response.status)
    }) {
        return false;
    }
    state.failed_document = Some(request_id.to_owned());
    true
}

/// Stop actively watching `page` once its watch has ended. Target ownership remains until Chrome
/// destroys it or a new watch atomically takes the root, so every request stays fail-closed. ~keep
fn release(shared: &Shared, page: &Arc<WatchedPage>, _keep_root: bool) {
    let mut registry = lock(&shared.registry);
    registry.pages.retain(|watched| !Arc::ptr_eq(watched, page));
    if registry
        .default_context_owner
        .as_ref()
        .is_some_and(|owner| Arc::ptr_eq(owner, page))
    {
        registry.default_context_owner = None;
    }
    registry
        .frames
        .retain(|_, cached| !cached.owner.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, page)));
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
    begin_ending(&page);
    let keep_root = !close_page;
    // ~keep Taken out of the registry first, so the destroy events do not dispose it again.
    let context = close_page
        .then(|| lock(&shared.registry).opened.remove(&page.root))
        .flatten()
        .flatten();
    let disposed = if let Some(context) = context {
        dispose_and_retire_context(browser, shared, context).await
    } else {
        false
    };
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
                #[cfg(test)]
                tokio::time::sleep(shared.delays.close).await;
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

/// Who a request belongs to, found through the frame that sent it. A missing event-index entry
/// gets one bounded, deterministic lookup; timeout or overflow remains unknown and fail-closed.
async fn attribute(browser: &Browser, shared: &Shared, frame: &FrameId) -> Option<Owner> {
    {
        let registry = lock(&shared.registry);
        if matches!(
            registry.ownership.get(&TargetId::new(frame.inner())),
            Some(TargetOwnership::Quarantined | TargetOwnership::PendingContext(_) | TargetOwnership::PendingShared)
        ) {
            return None;
        }
        if let Some(owner) = registry.owner_of_frame(frame) {
            return Some(owner);
        }
    }
    let live: Vec<TargetId> = {
        let registry = lock(&shared.registry);
        registry
            .targets
            .iter()
            .map(|(id, _)| id.clone())
            .chain(registry.other_candidates.iter().cloned())
            .take(FRAME_ATTRIBUTION_TARGET_LIMIT)
            .collect()
    };
    #[cfg(test)]
    if let Some(gate) = &shared.delays.attribute_snapshot_gate {
        lock(&gate.held).push(frame.inner().clone());
        if let Ok(permit) = gate.permits.acquire().await {
            permit.forget();
        }
    }
    tokio::time::timeout(FRAME_ATTRIBUTION_TIMEOUT, async {
        for target in live {
            let Ok(page) = browser.get_page(target.clone()).await else {
                continue;
            };
            let Ok(frames) = page.frames().await else {
                continue;
            };
            if frames.contains(frame) {
                // ~keep Ownership can change across the CDP awaits above; cache only the current
                // ~keep owner, or a reused root can retain its preceding watch's policy.
                let mut registry = lock(&shared.registry);
                let owner = registry.owner_of_target(target.inner());
                let current = if owner.is_some() || registry.others.contains(&target) {
                    registry.current_frame_owner(target, owner)
                } else {
                    None
                };
                let Some(current) = current else {
                    continue;
                };
                let owner = current
                    .owner
                    .as_ref()
                    .map_or(Owner::Other, |owner| Owner::Watched(Arc::clone(owner)));
                registry.frames.insert(frame.clone(), current);
                return Some(owner);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Answer one paused request. It is judged by the policy of the watched page it belongs to,
/// and refused when its page's watch is ending. A request of another client's page on an
/// external browser is continued untouched; any other request is refused. A document response
/// of a watched page's main frame is judged by [`main_frame_verdict`], and a main-frame document
/// request the policy allows by [`navigation_verdict`].
async fn answer(browser: &Browser, shared: &Shared, event: &EventRequestPaused, paused: Paused<'_>) {
    let paused_at = paused.at;
    #[cfg(test)]
    if let Some((gate, prefix)) = &shared.delays.match_gate
        && event.request.url.starts_with(prefix.as_str())
    {
        lock(&gate.held).push(event.request.url.clone());
        if let Ok(permit) = gate.permits.acquire().await {
            permit.forget();
        }
    }
    // ~keep `_in_flight` lives to the end of this function, so the page counts the request until
    // ~keep its answer has been sent: a watch ending on a zero count has nothing still paused.
    let attributed = attribute(browser, shared, &event.frame_id).await;
    let (verdict, _in_flight) = match attributed {
        Some(Owner::Watched(page)) => {
            let in_flight = paused.matched(page);
            let page = &in_flight.0;
            let verdict = judge(shared, page, event, paused_at).await;
            let verdict = if page.ending.load(Ordering::Acquire) {
                Verdict::Refuse
            } else {
                verdict
            };
            if matches!(verdict, Verdict::Abort) {
                lock(&page.outcome).navigation_dropped = true;
            }
            (verdict, Some(in_flight))
        }
        owner => {
            drop(paused);
            match owner {
                Some(Owner::Other) if shared.origin == BrowserOrigin::External => (Verdict::Continue(None), None),
                _ => (Verdict::Refuse, None),
            }
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
    let _ = match verdict {
        Verdict::RenderRedirect | Verdict::Stop => {
            let result = fulfill_inert_document(browser, request_id.clone()).await;
            if result.is_ok() {
                complete_stopped_response(_in_flight.as_ref(), &request_id, None);
            }
            result
        }
        Verdict::Continue(_) if is_response_stage(event) => {
            browser.execute(ContinueResponseParams::new(request_id)).await.map(drop)
        }
        Verdict::Continue(headers) => {
            let mut params = ContinueRequestParams::new(request_id);
            params.headers = headers;
            browser.execute(params).await.map(drop)
        }
        Verdict::Refuse => browser
            .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
            .await
            .map(drop),
        // ~keep Chrome commits no error page for an aborted navigation, so the page keeps the
        // ~keep document it has: all of it, or the part it had parsed when a script navigated.
        Verdict::Abort => browser
            .execute(FailRequestParams::new(request_id, ErrorReason::Aborted))
            .await
            .map(drop),
    };
}

/// How the check answers one paused request.
enum Verdict {
    /// Let it go out, with these headers in place of its own when set.
    Continue(Option<Vec<HeaderEntry>>),
    /// ~keep Replace a terminal redirect with a complete empty inert document.
    RenderRedirect,
    /// ~keep Replace a response that cannot commit with an inert document, then report it as sent.
    Stop,
    /// Fail it with `BlockedByClient`.
    Refuse,
    /// ~keep Drop a navigation the page must not wait on, so it keeps its document.
    Abort,
}

async fn fulfill_inert_document(
    browser: &Browser,
    request_id: FetchRequestId,
) -> Result<(), chromiumoxide::error::CdpError> {
    let mut params = FulfillRequestParams::new(request_id, 200);
    params.response_phrase = Some("OK".to_owned());
    params.response_headers = Some(vec![
        HeaderEntry::new("Content-Type", "text/plain; charset=utf-8"),
        HeaderEntry::new("Content-Length", "0"),
        HeaderEntry::new("Content-Security-Policy", "default-src 'none'; sandbox"),
        HeaderEntry::new("X-Content-Type-Options", "nosniff"),
    ]);
    params.body = Some(String::new().into());
    browser.execute(params).await.map(drop)
}

fn complete_stopped_response(in_flight: Option<&InFlight>, request_id: &FetchRequestId, body: Option<Vec<u8>>) {
    let Some(request) = in_flight else { return };
    complete_stopped_response_outcome(&request.0.outcome, request_id, body);
}

fn complete_stopped_response_outcome(
    outcome: &Mutex<InterceptOutcome>,
    request_id: &FetchRequestId,
    body: Option<Vec<u8>>,
) {
    let mut outcome = lock(outcome);
    if let Some(stop) = outcome.stopped_response.as_mut()
        && stop.request_id.as_ref() == Some(request_id)
    {
        if let Some(body) = body {
            stop.body = String::from_utf8_lossy(&body).into_owned();
            stop.body_bytes = body;
        }
        stop.ready = true;
    }
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
        return main_frame_verdict(event, &page.main_frame, page.redirect_limit, &page.outcome);
    }
    #[cfg(test)]
    {
        tokio::time::sleep(shared.delays.verdict).await;
        if let Some(gate) = &shared.delays.verdict_gate {
            lock(&gate.held).push(event.request.url.clone());
            let _ = gate.permits.acquire().await;
        }
    }
    let (url, reason) = match ssrf_verdict_for_browser(
        &event.request.url,
        &page.config.ssrf,
        shared.origin.resolves_target_remotely(),
    )
    .await
    {
        Ok(parsed) => {
            if !navigation_verdict(event, &page.main_frame, page.redirect_limit, &page.outcome) {
                return Verdict::Abort;
            }
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
#[cfg(test)]
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<url::Url, (String, String)> {
    ssrf_verdict_for_browser(request_url, policy, false).await
}

async fn ssrf_verdict_for_browser(
    request_url: &str,
    policy: &SsrfPolicy,
    remote_resolution: bool,
) -> Result<url::Url, (String, String)> {
    let parsed = url::Url::parse(request_url).map_err(|e| (UNPARSEABLE_URL.to_owned(), format!("invalid URL: {e}")))?;
    if userinfo::has_userinfo(&parsed) {
        let mut clean = parsed;
        userinfo::strip(&mut clean);
        return Err((clean.into(), "a URL with credentials in it is refused".to_owned()));
    }
    if remote_resolution {
        crate::net::ssrf::validate_remote_resolution(&parsed, policy)
            .map_err(|e| (parsed.to_string(), e.to_string()))?;
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

fn has_fetchable_redirect_target(response_url: &str, headers: &[HeaderEntry]) -> bool {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("location"))
        .and_then(|header| resolve_redirect(response_url, &header.value))
        .is_some_and(|target| is_fetchable_scheme(&target))
}

/// How a paused document response is answered, recording the status and headers of each
/// main-frame document response. Only the response of the committed document is kept beside the
/// new one. A main-frame redirect is counted while it is within `limit`. Past the limit, a
/// redirect of the requested navigation is recorded and rendered without its `Location`, as the
/// HTTP fetch stops on it; a redirect of a navigation the page started is dropped. A main-frame
/// response of the requested navigation that Chrome does not commit is also recorded and failed.
///
/// ~keep Such a response from a later navigation is dropped, so it cannot keep the requested
/// ~keep navigation waiting for a load event that will never arrive.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started.
/// ~keep Chrome commits no document for a 204, 205 or 304, so no load event fires and
/// ~keep chromiumoxide's `goto` waits for the browser timeout. An empty inert internal document
/// ~keep ends `goto`; callers still receive the original no-document response.
/// ~keep A redirect to a non-web address cannot produce another paused request for the listener.
/// ~keep At the response headers, it is fulfilled as a complete empty internal 200 with a
/// ~keep sandboxed `text/plain` header set. Chrome can neither wait on the origin body, follow
/// ~keep the non-web `Location`, nor execute content; callers receive the original status,
/// ~keep headers and URL with an empty body.
fn main_frame_verdict(
    event: &EventRequestPaused,
    main_frame: &FrameId,
    limit: usize,
    state: &Mutex<InterceptOutcome>,
) -> Verdict {
    if *main_frame != event.frame_id {
        return Verdict::Continue(None);
    }
    let mut state = lock(state);
    if state
        .stopped_response
        .as_ref()
        .is_some_and(|response| response.request_id.as_ref() != Some(&event.request_id))
    {
        state.stopped_response = None;
    }
    let headers = event.response_headers.as_deref().unwrap_or_default();
    let status = event.response_status_code.and_then(|code| u16::try_from(code).ok());
    let is_redirect = status.is_some_and(|code| REDIRECT_STATUSES.contains(&code))
        && headers.iter().any(|h| h.name.eq_ignore_ascii_case("location"));
    if let Some(status) = status
        && let Some(network_id) = &event.network_id
    {
        record_main_frame_response(
            &mut state,
            network_id.as_ref(),
            &event.request.url,
            status,
            is_redirect,
            headers,
        );
    }
    let mut render_redirect = false;
    let stop = match status {
        Some(code) if is_redirect => {
            if !has_fetchable_redirect_target(&event.request.url, headers) {
                render_redirect = true;
                code
            } else if spend_redirect(&mut state, limit) {
                return Verdict::Continue(None);
            } else if state.first_document_arrived {
                return Verdict::Abort;
            } else {
                render_redirect = true;
                code
            }
        }
        Some(code) if NO_DOCUMENT_STATUSES.contains(&code) => {
            if state.first_document_arrived {
                return Verdict::Abort;
            }
            code
        }
        _ => {
            state.first_document_arrived = true;
            return Verdict::Continue(None);
        }
    };
    state.stopped_response = Some(StoppedResponse {
        url: crate::net::redact_url_credentials(&event.request.url),
        status: stop,
        headers: header_map(headers),
        body: String::new(),
        body_bytes: Vec::new(),
        request_id: Some(event.request_id.clone()),
        terminal_intercepted: render_redirect,
        ready: false,
    });
    if render_redirect {
        Verdict::RenderRedirect
    } else {
        Verdict::Stop
    }
}

/// Whether a request the policy allows may go out. The first main-frame document request is the
/// requested navigation. Every later one that is not a redirect hop is a navigation the page
/// started (a meta refresh, a script, a form), and it is counted as one redirect: past `limit` it
/// must be dropped, so the page keeps its document. A redirect hop was counted at its response.
fn navigation_verdict(
    event: &EventRequestPaused,
    main_frame: &FrameId,
    limit: usize,
    state: &Mutex<InterceptOutcome>,
) -> bool {
    if *main_frame != event.frame_id || event.resource_type != ResourceType::Document {
        return true;
    }
    let mut state = lock(state);
    state.latest_document_request = event.network_id.as_ref().map(|id| id.as_ref().to_owned());
    state.failed_document = None;
    if event.redirected_request_id.is_some() {
        return true;
    }
    if !state.navigation_started {
        state.navigation_started = true;
        return true;
    }
    spend_redirect(&mut state, limit)
}

/// Count one redirect against `limit`, or return false when the limit is spent. Once the
/// requested navigation has ended nothing is counted.
fn spend_redirect(state: &mut InterceptOutcome, limit: usize) -> bool {
    if state.navigation_ended {
        return true;
    }
    if state.redirects_followed >= limit {
        return false;
    }
    state.redirects_followed += 1;
    true
}

/// Record a main-frame response of the navigation `network_id`: a redirect adds to that
/// navigation's count, and any other response is recorded as a document with the count.
///
/// ~keep Chrome keeps one network request id across the redirects of a navigation, so the count
/// ~keep is the navigation's own. A late navigation does not inherit the redirects of the seed.
/// ~keep A newer main-frame response cancels a navigation that has not committed, so only the
/// ~keep committed document and this response can still be the one the page shows. Without the
/// ~keep pruning a page that keeps navigating to a 204 grows the map.
fn record_main_frame_response(
    state: &mut InterceptOutcome,
    network_id: &str,
    response_url: &str,
    status: u16,
    is_redirect: bool,
    headers: &[HeaderEntry],
) {
    state.failed_document = None;
    state.latest_document_request = Some(network_id.to_owned());
    let redirects = match state.pending_redirects.take() {
        Some((pending, count)) if pending == network_id => count,
        _ => 0,
    };
    if is_redirect {
        state.pending_redirects = Some((network_id.to_owned(), redirects + 1));
        return;
    }
    if let Some(committed) = state.committed_loader.clone() {
        state.documents.retain(|id, _| *id == committed);
    }
    state.documents.insert(
        network_id.to_owned(),
        DocumentResponse {
            url: crate::net::redact_url_credentials(response_url),
            status,
            headers: header_map(headers),
            redirects,
        },
    );
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use futures::StreamExt as _;
    use tokio::sync::{Notify, mpsc, oneshot};

    use super::{
        BrowserContextId, BrowserOrigin, CLOSE_TIMEOUT, EventLoadingFailed, EventRequestPaused, EventTargetCreated,
        FIREWALL_STOP_TIMEOUT, FetchRequestId, FrameId, HeaderEntry, InterceptOutcome, Opening, Owner, PageContext,
        Registry, Shared, TargetId, TargetOwnership, TestDelays, Verdict, WatchedPage, adopt_target, begin_ending,
        child_policy_acknowledged, complete_stopped_response_outcome, controller_completed, events_of,
        failed_document_response, install_watch, judge, lock, main_frame_verdict, navigation_verdict,
        reconcile_created, reconcile_destroyed, record_document_failure_outcome, record_main_frame_commit,
        registered_context, release, require_main_frame, serve_child_targets_with_timeout, ssrf_verdict,
        ssrf_verdict_for_browser,
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

    #[test]
    fn firewall_stop_outlives_its_bounded_context_disposal_census() {
        assert!(FIREWALL_STOP_TIMEOUT > CLOSE_TIMEOUT);
    }

    #[test]
    fn pooled_cookie_lookup_uses_only_the_page_s_registered_context() {
        let first = TargetId::new("FIRST");
        let second = TargetId::new("SECOND");
        let shared = TargetId::new("SHARED");
        let missing = TargetId::new("MISSING");
        let first_context = BrowserContextId::new("CONTEXT-1");
        let second_context = BrowserContextId::new("CONTEXT-2");
        let mut registry = Registry::default();
        registry.opened.insert(first.clone(), Some(first_context.clone()));
        registry.opened.insert(second, Some(second_context));
        registry.opened.insert(shared.clone(), None);

        assert_eq!(
            registered_context(&registry, &first).expect("first context"),
            Some(first_context)
        );
        assert_eq!(
            registered_context(&registry, &shared).expect("shared context entry"),
            None,
            "a present shared-context entry is distinct from a missing page"
        );
        assert!(
            registered_context(&registry, &missing).is_err(),
            "an unregistered page must not fall back to the default browser jar"
        );
    }

    #[tokio::test]
    async fn rejects_loopback_navigation() {
        let verdict = ssrf_verdict("http://127.0.0.1/admin", &deny_policy()).await;
        assert!(verdict.is_err(), "loopback must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_rfc1918_and_loopback_addresses() {
        for url in [
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.0.1/",
            "http://[::1]/",
        ] {
            let verdict = ssrf_verdict(url, &deny_policy()).await;
            assert!(
                verdict.is_err(),
                "private address must be rejected for {url}: {verdict:?}"
            );
        }
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

    #[tokio::test]
    async fn allows_a_private_address_named_by_the_allowlist() {
        let policy = SsrfPolicy {
            allowlist: vec![
                crate::net::ssrf::HostMatcher::cidr("127.0.0.1/32").expect("the literal-IP CIDR must be valid"),
            ],
            ..deny_policy()
        };
        let verdict = ssrf_verdict("http://127.0.0.1/", &policy).await;
        assert!(
            verdict.is_ok(),
            "the explicit allowlist must override the private-address denial"
        );
    }

    #[tokio::test]
    async fn external_browser_refuses_hostname_when_custom_denials_cannot_be_bound_to_its_lookup() {
        let policy = SsrfPolicy {
            deny_private: false,
            denylist: vec![crate::net::ssrf::HostMatcher::cidr("203.0.113.0/24").expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        };

        let error = ssrf_verdict_for_browser("http://target.example/", &policy, true)
            .await
            .expect_err("an external browser performs a later, unbound hostname lookup");
        assert_eq!(error.1, "denied by SSRF policy: configured_network");

        let literal = ssrf_verdict_for_browser("http://198.51.100.1/", &policy, true).await;
        assert!(
            literal.is_ok(),
            "a permitted literal address remains decidable at the external-browser boundary: {literal:?}"
        );

        let no_custom_denials = SsrfPolicy {
            deny_private: false,
            allowlist: vec![crate::net::ssrf::HostMatcher::exact("target.example")],
            ..SsrfPolicy::default()
        };
        let hostname = ssrf_verdict_for_browser("http://target.example/", &no_custom_denials, true).await;
        assert!(
            hostname.is_ok(),
            "remote hostname resolution is refused specifically when custom networks must be enforced: {hostname:?}"
        );
    }

    #[tokio::test]
    async fn external_browser_judges_the_remote_resolution_boundary_before_navigation() {
        let mut page = watched("MAIN");
        Arc::get_mut(&mut page).expect("the fixture has one owner").config.ssrf = SsrfPolicy {
            allowlist: vec![crate::net::ssrf::HostMatcher::exact("localhost")],
            denylist: vec![crate::net::ssrf::HostMatcher::cidr("203.0.113.0/24").expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        };
        let shared = shared_with(&page, "OTHER");
        let event = main_frame_request("http://localhost/", false);

        let verdict = judge(&shared, &page, &event, Instant::now()).await;

        assert!(matches!(verdict, Verdict::Refuse));
        let outcome = page.outcome.lock().expect("outcome lock");
        assert_eq!(
            outcome.blocked.as_ref().map(|(_, reason)| reason.as_str()),
            Some("denied by SSRF policy: configured_network"),
            "without the external-browser boundary, localhost is admitted by its exact allowlist"
        );
    }

    /// A watched page whose own target is `root`.
    fn watched(root: &str) -> Arc<WatchedPage> {
        Arc::new(WatchedPage {
            root: TargetId::new(root),
            main_frame: FrameId::new(root),
            config: crate::types::CrawlConfig::default(),
            redirect_limit: 0,
            outcome: Mutex::new(InterceptOutcome::default()),
            refusals: Mutex::new(Vec::new()),
            refused_urls: Mutex::new(Vec::new()),
            refused_count: AtomicUsize::new(0),
            ending: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            document_failed: Notify::new(),
            events_ended: Notify::new(),
        })
    }

    #[tokio::test]
    async fn a_parked_watch_ends_its_failure_stream_without_another_event() {
        use futures::StreamExt as _;

        let page = watched("ROOT");
        let weak = Arc::downgrade(&page);
        let pending = futures::stream::pending::<Arc<EventLoadingFailed>>();
        let mut failures = events_of(&page, pending);

        begin_ending(&page);
        let ended = tokio::time::timeout(Duration::from_millis(100), failures.next()).await;
        assert!(
            matches!(ended, Ok(None)),
            "the page-scoped failure stream stayed pending"
        );
        drop(failures);
        drop(page);
        assert!(weak.upgrade().is_none(), "the ended stream retained the watched page");
    }

    /// The listener's state on an external browser with `page` watched and another client's
    /// target `other` open.
    fn shared_with(page: &Arc<WatchedPage>, other: &str) -> Shared {
        let mut registry = Registry {
            pages: vec![Arc::clone(page)],
            ..Registry::default()
        };
        registry.register_watched(page.root.clone(), Arc::clone(page));
        registry.register_other(TargetId::new(other));
        Shared {
            registry: Mutex::new(registry),
            destroyed: Notify::new(),
            origin: BrowserOrigin::External,
            context: PageContext::Copied,
            unmatched: Mutex::new(Vec::new()),
            opening: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            failure: Notify::new(),
            handler_abort: None,
            delays: TestDelays::default(),
        }
    }

    type TestChildSocket =
        async_tungstenite::WebSocketStream<async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>>;

    struct StalledIo;

    impl futures::io::AsyncRead for StalledIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            _buffer: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }
    }

    impl futures::io::AsyncWrite for StalledIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            _buffer: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    async fn test_child_controller(
        shared: Arc<Shared>,
        command_timeout: Duration,
    ) -> (
        TestChildSocket,
        mpsc::Sender<super::ChildCommand>,
        tokio::task::JoinHandle<bool>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind child controller test socket");
        let address = listener.local_addr().expect("test socket address");
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept child controller");
            stream.set_nodelay(true).expect("disable test server Nagle delay");
            async_tungstenite::tokio::accept_async(stream)
                .await
                .expect("accept WebSocket")
        });
        let (client, _) = async_tungstenite::tokio::connect_async(format!("ws://{address}"))
            .await
            .expect("connect child controller WebSocket");
        client
            .get_ref()
            .get_ref()
            .set_nodelay(true)
            .expect("disable test client Nagle delay");
        let mut server = accepted.await.expect("WebSocket accept task");
        let (commands, mut receiver) = mpsc::channel(super::CHILD_CONTROLLER_PENDING_LIMIT);
        let controller = tokio::spawn(async move {
            serve_child_targets_with_timeout(client, &mut receiver, &shared, command_timeout).await
        });
        let (done, enabled) = oneshot::channel();
        commands
            .send(super::ChildCommand::Enable { done })
            .await
            .expect("enable child controller");
        let global = next_child_message(&mut server).await;
        assert_eq!(global["method"], "Target.setAutoAttach");
        assert_eq!(global["params"]["waitForDebuggerOnStart"], true);
        assert_eq!(
            global["params"]["filter"],
            serde_json::json!([
                { "type": "browser", "exclude": true },
                { "type": "page", "exclude": true },
                {},
            ])
        );
        answer_child_command(&mut server, &global).await;
        enabled
            .await
            .expect("global arm sender")
            .expect("global arm acknowledgement");
        (server, commands, controller)
    }

    async fn next_child_message(socket: &mut TestChildSocket) -> serde_json::Value {
        let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("child controller must send within the test bound")
            .expect("child controller socket must stay open")
            .expect("child controller message must be valid");
        let async_tungstenite::tungstenite::Message::Text(text) = message else {
            panic!("child controller must send text commands")
        };
        serde_json::from_str(text.as_ref()).expect("child controller command JSON")
    }

    async fn answer_child_command(socket: &mut TestChildSocket, command: &serde_json::Value) {
        socket
            .send(async_tungstenite::tungstenite::Message::Text(
                serde_json::json!({ "id": command["id"], "result": {} })
                    .to_string()
                    .into(),
            ))
            .await
            .expect("answer child controller command");
    }

    async fn begin_root_arm(socket: &mut TestChildSocket, context: &BrowserContextId) -> serde_json::Value {
        send_attached_target_of_type(socket, "S-ROOT", "ROOT", "page", None, context).await;
        let recursive = next_child_message(socket).await;
        assert_eq!(recursive["method"], "Target.setAutoAttach");
        assert_eq!(recursive["sessionId"], "S-ROOT");
        recursive
    }

    async fn finish_root_arm(
        socket: &mut TestChildSocket,
        arm: &serde_json::Value,
        armed: oneshot::Receiver<Result<(), String>>,
    ) {
        answer_child_command(socket, arm).await;
        armed.await.expect("root arm sender").expect("root arm acknowledgement");
        let resume = next_child_message(socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resume["sessionId"], "S-ROOT");
        answer_child_command(socket, &resume).await;
    }

    async fn send_attached_target(
        socket: &mut TestChildSocket,
        session: &str,
        target: &str,
        opener: Option<&str>,
        context: &BrowserContextId,
    ) {
        send_attached_target_of_type(socket, session, target, "worker", opener, context).await;
    }

    async fn send_attached_target_of_type(
        socket: &mut TestChildSocket,
        session: &str,
        target: &str,
        target_type: &str,
        opener: Option<&str>,
        context: &BrowserContextId,
    ) {
        send_attached_target_with_waiting(socket, session, target, target_type, opener, context, true).await;
    }

    async fn send_attached_target_with_waiting(
        socket: &mut TestChildSocket,
        session: &str,
        target: &str,
        target_type: &str,
        opener: Option<&str>,
        context: &BrowserContextId,
        waiting_for_debugger: bool,
    ) {
        send_attached_target_from_parent(
            socket,
            session,
            target,
            target_type,
            opener,
            context,
            waiting_for_debugger,
            None,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_attached_target_from_parent(
        socket: &mut TestChildSocket,
        session: &str,
        target: &str,
        target_type: &str,
        opener: Option<&str>,
        context: &BrowserContextId,
        waiting_for_debugger: bool,
        parent_session: Option<&str>,
    ) {
        let mut event = serde_json::json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": session,
                "targetInfo": {
                    "targetId": target,
                    "type": target_type,
                    "title": "",
                    "url": "http://a.localhost/worker.js",
                    "attached": true,
                    "canAccessOpener": false,
                    "openerId": opener,
                    "browserContextId": context.inner(),
                },
                "waitingForDebugger": waiting_for_debugger,
            }
        });
        if let Some(parent_session) = parent_session {
            event["sessionId"] = parent_session.into();
        }
        socket
            .send(async_tungstenite::tungstenite::Message::Text(event.to_string().into()))
            .await
            .expect("send attached target");
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

    fn target_created_in_context(id: &str, opener: Option<&str>, context: &BrowserContextId) -> EventTargetCreated {
        serde_json::from_value(serde_json::json!({
            "targetInfo": {
                "targetId": id,
                "type": "page",
                "title": "",
                "url": "http://a.localhost/frame",
                "attached": false,
                "canAccessOpener": false,
                "openerId": opener,
                "browserContextId": context.inner(),
            }
        }))
        .expect("a target created event with a browser context")
    }

    fn in_context(mut event: EventTargetCreated, context: &BrowserContextId) -> EventTargetCreated {
        event.target_info.browser_context_id = Some(context.clone());
        event
    }

    fn register_owned_context(shared: &Shared, page: &Arc<WatchedPage>) -> BrowserContextId {
        let context = BrowserContextId::new("OWNED-CONTEXT");
        let mut registry = lock(&shared.registry);
        registry.register_context(context.clone(), Some(&page.config.ssrf));
        registry
            .contexts
            .get_mut(&context)
            .expect("the context must be registered")
            .owner = Some(Arc::clone(page));
        registry.opened.insert(page.root.clone(), Some(context.clone()));
        registry.context_targets.insert(page.root.clone(), context.clone());
        context
    }

    #[tokio::test]
    async fn an_existing_running_target_before_the_global_ack_does_not_stop_the_controller() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind child controller test socket");
        let address = listener.local_addr().expect("test socket address");
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept child controller");
            async_tungstenite::tokio::accept_async(stream)
                .await
                .expect("accept WebSocket")
        });
        let (client, _) = async_tungstenite::tokio::connect_async(format!("ws://{address}"))
            .await
            .expect("connect child controller WebSocket");
        let mut socket = accepted.await.expect("WebSocket accept task");
        let (commands, mut receiver) = mpsc::channel(super::CHILD_CONTROLLER_PENDING_LIMIT);
        let controller = tokio::spawn(async move {
            serve_child_targets_with_timeout(client, &mut receiver, &shared, Duration::from_millis(250)).await
        });
        let (done, enabled) = oneshot::channel();
        commands
            .send(super::ChildCommand::Enable { done })
            .await
            .expect("enable child controller");
        let global = next_child_message(&mut socket).await;
        send_attached_target_with_waiting(
            &mut socket,
            "S-OTHER",
            "OTHER",
            "page",
            None,
            &BrowserContextId::new("DEFAULT"),
            false,
        )
        .await;
        answer_child_command(&mut socket, &global).await;
        enabled
            .await
            .expect("global arm sender")
            .expect("global arm acknowledgement");

        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn an_existing_running_owned_target_is_recursively_armed_without_being_resumed() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        send_attached_target_with_waiting(&mut socket, "S-ROOT", "ROOT", "page", None, &context, false).await;
        let recursive_arm = next_child_message(&mut socket).await;
        assert_eq!(recursive_arm["method"], "Target.setAutoAttach");
        assert_eq!(recursive_arm["sessionId"], "S-ROOT");
        assert_eq!(
            recursive_arm["params"]["filter"],
            serde_json::json!([
                { "type": "browser", "exclude": true },
                { "type": "tab", "exclude": true },
                {},
            ])
        );
        answer_child_command(&mut socket, &recursive_arm).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "a target that was already running must not receive a redundant resume"
        );

        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm existing root");
        armed
            .await
            .expect("root arm sender")
            .expect("existing recursive boundary");
        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn an_ending_owned_root_under_a_paused_tab_is_not_closed_or_resumed() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        begin_ending(&page);
        let (mut socket, _commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &context, true).await;
        let arm = next_child_message(&mut socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach");
        send_attached_target_from_parent(
            &mut socket,
            "S-ROOT",
            "ROOT",
            "page",
            None,
            &context,
            false,
            Some("S-TAB"),
        )
        .await;
        assert!(!controller.await.expect("the controller must fail closed"));
        let remaining = tokio::time::timeout(Duration::from_millis(100), socket.next())
            .await
            .expect("the controller socket must end");
        assert!(
            !matches!(remaining, Some(Ok(async_tungstenite::tungstenite::Message::Text(_)))),
            "an ending owned root must receive neither a close nor a resume command"
        );
        assert!(lock(&shared.registry).opened.contains_key(&page.root));
    }

    #[tokio::test]
    async fn an_ending_watch_popup_under_a_paused_tab_is_closed_without_resuming() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        begin_ending(&page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &context, true).await;
        let tab_arm = next_child_message(&mut socket).await;
        send_attached_target_from_parent(
            &mut socket,
            "S-POPUP",
            "POPUP",
            "page",
            Some("ROOT"),
            &context,
            false,
            Some("S-TAB"),
        )
        .await;
        let close = next_child_message(&mut socket).await;
        assert_eq!(close["method"], "Target.closeTarget");
        assert_eq!(close["params"]["targetId"], "POPUP");
        answer_child_command(&mut socket, &tab_arm).await;
        answer_child_command(&mut socket, &close).await;
        for (session, target) in [("S-POPUP", "POPUP"), ("S-TAB", "TAB")] {
            socket
                .send(async_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "method": "Target.detachedFromTarget", "params": { "sessionId": session, "targetId": target },
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("the popup page and tab must detach");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err()
        );
        assert!(lock(&shared.registry).opened.contains_key(&page.root));
        commands.send(super::ChildCommand::Stop).await.expect("stop controller");
        assert!(controller.await.expect("controller task"));
    }

    #[tokio::test]
    async fn a_structural_tab_stays_paused_until_its_context_bearing_child_boundary_is_acknowledged() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;

        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &context, true).await;
        let tab_arm = next_child_message(&mut socket).await;
        assert_eq!(tab_arm["method"], "Target.setAutoAttach");
        assert_eq!(tab_arm["sessionId"], "S-TAB");
        assert_eq!(
            tab_arm["params"]["filter"],
            serde_json::json!([
                { "type": "browser", "exclude": true },
                { "type": "tab", "exclude": true },
                {},
            ])
        );
        send_attached_target_from_parent(
            &mut socket,
            "S-PAGE",
            "PAGE",
            "page",
            None,
            &context,
            false,
            Some("S-TAB"),
        )
        .await;
        let page_arm = next_child_message(&mut socket).await;
        assert_eq!(page_arm["method"], "Target.setAutoAttach");
        assert_eq!(page_arm["sessionId"], "S-PAGE");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "the child resumed before its recursive interception boundary was acknowledged"
        );
        answer_child_command(&mut socket, &page_arm).await;
        let page_resume = next_child_message(&mut socket).await;
        assert_eq!(page_resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(page_resume["sessionId"], "S-PAGE");
        answer_child_command(&mut socket, &page_resume).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "the tab resumed before its own recursive boundary was acknowledged"
        );

        answer_child_command(&mut socket, &tab_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resume["sessionId"], "S-TAB");
        answer_child_command(&mut socket, &resume).await;
        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_structural_tab_without_a_child_boundary_fails_within_the_controller_deadline() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, _commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(40)).await;

        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &context, true).await;
        let tab_arm = next_child_message(&mut socket).await;
        assert_eq!(tab_arm["method"], "Target.setAutoAttach");
        answer_child_command(&mut socket, &tab_arm).await;

        assert!(
            !tokio::time::timeout(Duration::from_millis(250), controller)
                .await
                .expect("structural child deadline")
                .expect("child controller task"),
            "a structural target without an authoritative child boundary must fail closed"
        );
    }

    #[tokio::test]
    async fn a_nested_child_stays_paused_until_each_recursive_boundary_is_acknowledged() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;

        for (session, target, opener) in [("S-CHILD", "CHILD", "ROOT"), ("S-NESTED", "NESTED", "CHILD")] {
            send_attached_target(&mut socket, session, target, Some(opener), &context).await;
            let recursive_arm = next_child_message(&mut socket).await;
            assert_eq!(recursive_arm["method"], "Target.setAutoAttach");
            assert_eq!(recursive_arm["sessionId"], session);
            assert!(
                tokio::time::timeout(Duration::from_millis(25), socket.next())
                    .await
                    .is_err(),
                "{target} resumed before its recursive target boundary was acknowledged"
            );
            answer_child_command(&mut socket, &recursive_arm).await;
            let resume = next_child_message(&mut socket).await;
            assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
            assert_eq!(resume["sessionId"], session);
            answer_child_command(&mut socket, &resume).await;
        }

        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn an_openerless_owned_target_is_caught_by_the_browser_wide_boundary() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;

        send_attached_target(&mut socket, "S-NOOPENER", "NOOPENER", None, &context).await;
        let recursive_arm = next_child_message(&mut socket).await;
        assert_eq!(recursive_arm["method"], "Target.setAutoAttach");
        assert_eq!(recursive_arm["sessionId"], "S-NOOPENER");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "the openerless target resumed before its recursive boundary was acknowledged"
        );
        answer_child_command(&mut socket, &recursive_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resume["sessionId"], "S-NOOPENER");
        answer_child_command(&mut socket, &resume).await;

        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_foreign_external_target_is_resumed_without_an_owned_policy() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;

        let foreign = BrowserContextId::new(format!("FOREIGN-{}", context.inner()));
        send_attached_target(&mut socket, "S-OTHER", "OTHER", None, &foreign).await;
        let recursive_arm = next_child_message(&mut socket).await;
        assert_eq!(recursive_arm["method"], "Target.setAutoAttach");
        assert_eq!(recursive_arm["sessionId"], "S-OTHER");
        answer_child_command(&mut socket, &recursive_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resume["sessionId"], "S-OTHER");
        answer_child_command(&mut socket, &resume).await;

        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn the_watched_root_resumes_after_its_existing_boundary_ack() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        send_attached_target_of_type(&mut socket, "S-ROOT", "ROOT", "page", None, &context).await;
        let root_arm = next_child_message(&mut socket).await;
        assert_eq!(root_arm["method"], "Target.setAutoAttach");
        assert_eq!(root_arm["sessionId"], "S-ROOT");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "the watched root resumed before its existing boundary was acknowledged"
        );
        answer_child_command(&mut socket, &root_arm).await;
        armed.await.expect("root arm sender").expect("root arm acknowledgement");
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resume["sessionId"], "S-ROOT");
        answer_child_command(&mut socket, &resume).await;

        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_missing_root_arm_acknowledgement_fails_within_the_controller_deadline() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(40)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let _arm = begin_root_arm(&mut socket, &context).await;

        let error = tokio::time::timeout(Duration::from_millis(250), armed)
            .await
            .expect("arm deadline")
            .expect("arm result sender")
            .expect_err("missing acknowledgement must fail the arm");
        assert!(error.contains("timed out"), "unexpected arm error: {error}");
        assert!(!controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_stalled_child_socket_write_fails_within_the_controller_deadline() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let socket = async_tungstenite::WebSocketStream::from_raw_socket(
            StalledIo,
            async_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let (commands, mut receiver) = mpsc::channel(super::CHILD_CONTROLLER_PENDING_LIMIT);
        let controller = tokio::spawn(async move {
            serve_child_targets_with_timeout(socket, &mut receiver, &shared, Duration::from_millis(40)).await
        });
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Enable { done })
            .await
            .expect("arm root");

        let error = tokio::time::timeout(Duration::from_millis(250), armed)
            .await
            .expect("socket write deadline")
            .expect("arm result sender")
            .expect_err("a stalled socket write must fail the arm");
        assert!(error.contains("timed out writing"), "unexpected write error: {error}");
        assert!(!controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_dropped_connection_fails_a_pending_root_arm() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let _arm = begin_root_arm(&mut socket, &context).await;
        socket.close(None).await.expect("drop controller connection");

        assert!(
            armed.await.is_err(),
            "a dropped connection must drop the pending arm reply"
        );
        assert!(!controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_missing_runtime_acknowledgement_fails_within_the_controller_deadline() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(40)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;
        send_attached_target(&mut socket, "S-CHILD", "CHILD", Some("ROOT"), &context).await;
        let recursive_arm = next_child_message(&mut socket).await;
        answer_child_command(&mut socket, &recursive_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");

        let ended = tokio::time::timeout(Duration::from_millis(250), controller)
            .await
            .expect("runtime acknowledgement deadline")
            .expect("child controller task");
        assert!(!ended, "missing Runtime acknowledgement must fail the controller");
    }

    #[tokio::test]
    async fn a_dropped_connection_fails_a_pending_runtime_resume() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;
        send_attached_target(&mut socket, "S-CHILD", "CHILD", Some("ROOT"), &context).await;
        let recursive_arm = next_child_message(&mut socket).await;
        answer_child_command(&mut socket, &recursive_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        socket.close(None).await.expect("drop controller connection");

        assert!(!controller.await.expect("child controller task"));
    }

    #[tokio::test]
    async fn a_cancelled_arm_caller_does_not_leave_a_pending_command() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(40)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        drop(armed);
        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(25), socket.next())
                .await
                .is_err(),
            "a cancelled root waiter must not emit a target command"
        );
        commands
            .send(super::ChildCommand::Stop)
            .await
            .expect("stop child controller");
        assert!(
            controller.await.expect("child controller task"),
            "a cancelled caller must not turn its abandoned acknowledgement into controller failure"
        );
    }

    #[tokio::test]
    async fn a_child_controller_panic_is_reported_as_unexpected_completion() {
        let completed = controller_completed(async { panic!("injected child-controller panic") }).await;
        assert!(!completed);
    }

    #[tokio::test]
    async fn a_request_listener_panic_is_reported_as_unexpected_completion() {
        let completed = controller_completed(async { panic!("injected request-listener panic") }).await;
        assert!(!completed);
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
        let context = register_owned_context(&shared, &page);
        let frame_event = in_context(target_created("FRAME", "iframe", None, None, Some("ROOT")), &context);
        let frame = adopt_target(&shared, &frame_event.target_info);
        let popup_event = in_context(
            target_created("POPUP", "page", Some("ROOT"), Some("FRAME"), None),
            &context,
        );
        let popup = adopt_target(&shared, &popup_event.target_info);
        page.ending.store(true, Ordering::Release);
        let late_event = in_context(
            target_created("LATE", "page", Some("ROOT"), Some("FRAME"), None),
            &context,
        );
        let late = adopt_target(&shared, &late_event.target_info);
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

    #[test]
    fn a_lineaged_target_without_an_authoritative_context_stays_fail_closed() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let frame = target_created("FRAME", "iframe", None, None, Some("ROOT"));
        let popup = target_created("POPUP", "page", Some("ROOT"), None, None);

        let close_frame = adopt_target(&shared, &frame.target_info);
        let close_popup = adopt_target(&shared, &popup.target_info);
        let registry = lock(&shared.registry);

        assert!(close_frame.is_none(), "an OOPIF must not be closed independently");
        assert_eq!(close_popup, Some(TargetId::new("POPUP")));
        assert!(
            matches!(
                registry.ownership.get(&TargetId::new("FRAME")),
                Some(TargetOwnership::Quarantined)
            ) && matches!(
                registry.ownership.get(&TargetId::new("POPUP")),
                Some(TargetOwnership::Quarantined)
            ),
            "both targets must be excluded from lineage fallback"
        );
        assert!(
            registry.owner_of_target("FRAME").is_none()
                && registry.owner_of_target("POPUP").is_none()
                && !registry.others.contains(&TargetId::new("FRAME"))
                && !registry.others.contains(&TargetId::new("POPUP")),
            "a missing context must inherit neither the opener's policy nor foreign-target privileges"
        );
        assert!(
            !child_policy_acknowledged(&registry, &TargetId::new("FRAME"))
                && !child_policy_acknowledged(&registry, &TargetId::new("POPUP")),
            "neither quarantined child may be resumed"
        );
    }

    #[test]
    fn an_absent_disposed_context_releases_its_owner_but_keeps_fail_closed_tombstones() {
        let page = watched("ROOT");
        let weak = Arc::downgrade(&page);
        let context = BrowserContextId::new("DISPOSED-CONTEXT");
        let target = TargetId::new("CHILD");
        let mut registry = Registry::default();
        registry.register_context(context.clone(), Some(&page.config.ssrf));
        registry
            .contexts
            .get_mut(&context)
            .expect("the context is registered")
            .owner = Some(Arc::clone(&page));
        registry.context_targets.insert(target.clone(), context.clone());
        registry.register_watched(target.clone(), Arc::clone(&page));

        assert!(
            !registry.retire_context(&context),
            "authoritative absence alone must not retire a context before disposal is acknowledged"
        );
        registry
            .contexts
            .get_mut(&context)
            .expect("the context is registered")
            .disposal_acknowledged = true;
        assert!(registry.retire_context(&context));
        drop(page);

        assert!(weak.upgrade().is_none(), "retirement must release every owner Arc");
        assert!(
            !registry.contexts.contains_key(&context) && registry.retired_contexts.contains(&context),
            "retirement must replace the full context policy with a minimal tombstone"
        );
        assert_eq!(
            registry.context_targets.get(&target),
            Some(&context),
            "the target-to-context census must remain for fail-closed accounting"
        );
        assert!(
            matches!(registry.ownership.get(&target), Some(TargetOwnership::Quarantined)),
            "a retired target must remain explicitly quarantined"
        );

        let shared = Shared {
            registry: Mutex::new(registry),
            destroyed: Notify::new(),
            origin: BrowserOrigin::External,
            context: PageContext::Copied,
            unmatched: Mutex::new(Vec::new()),
            opening: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            failure: Notify::new(),
            handler_abort: None,
            delays: TestDelays::default(),
        };
        let late = target_created_in_context("LATE", None, &context);
        let close = adopt_target(&shared, &late.target_info);
        assert_eq!(close, Some(TargetId::new("LATE")));
        assert!(
            matches!(
                lock(&shared.registry).ownership.get(&TargetId::new("LATE")),
                Some(TargetOwnership::Quarantined)
            ),
            "a late event from a retired context must remain fail-closed"
        );
    }

    #[test]
    fn repeated_context_retirement_keeps_no_full_policies_and_bounds_tombstones() {
        let page = watched("ROOT");
        let weak = Arc::downgrade(&page);
        let mut registry = Registry::default();
        let total = super::RETIRED_CONTEXT_LIMIT + 17;

        for index in 0..total {
            let context = BrowserContextId::new(format!("RETIRED-{index}"));
            registry.register_context(context.clone(), Some(&page.config.ssrf));
            let registered = registry.contexts.get_mut(&context).expect("the context is registered");
            registered.owner = Some(Arc::clone(&page));
            registered.disposal_acknowledged = true;
            assert!(registry.retire_context(&context));
        }
        drop(page);

        assert!(
            registry.contexts.is_empty(),
            "retired contexts must retain no full policy identity"
        );
        assert_eq!(registry.retired_contexts.len(), super::RETIRED_CONTEXT_LIMIT);
        assert!(
            !registry.retired_contexts.contains(&BrowserContextId::new("RETIRED-0"))
                && registry
                    .retired_contexts
                    .contains(&BrowserContextId::new(format!("RETIRED-{}", total - 1))),
            "the minimal retirement barrier must evict oldest ids and retain the newest"
        );
        assert!(weak.upgrade().is_none(), "retirement must not retain a policy owner");
    }

    #[test]
    fn a_parked_root_stays_fail_closed_until_the_next_watch_owns_it() {
        let old = watched("ROOT");
        let shared = shared_with(&old, "OTHER");
        let context = register_owned_context(&shared, &old);
        begin_ending(&old);
        release(&shared, &old, true);

        let late_event = in_context(target_created("LATE", "page", Some("ROOT"), None, None), &context);
        let late_popup = adopt_target(&shared, &late_event.target_info);
        let new = watched("ROOT");
        install_watch(&mut lock(&shared.registry), &new).expect("install watch");
        let registry = lock(&shared.registry);
        let root_owner = registry.owner_of_target("ROOT").expect("the root stays owned");
        let popup_owner = registry.owner_of_target("LATE").expect("the popup stays owned");

        assert_eq!(
            late_popup.as_ref().map(|id| id.inner().as_str()),
            Some("LATE"),
            "a popup created during handoff must be closed, not classified as another client's"
        );
        assert!(
            Arc::ptr_eq(&root_owner, &new) && !root_owner.ending.load(Ordering::Acquire),
            "the new policy must atomically take ownership of the parked root"
        );
        assert!(
            Arc::ptr_eq(&popup_owner, &old) && popup_owner.ending.load(Ordering::Acquire),
            "the late popup must remain fail-closed under the ending policy"
        );
        assert!(
            !registry.others.contains(&TargetId::new("ROOT")) && !registry.others.contains(&TargetId::new("LATE")),
            "neither the parked root nor its popup may become an external client's target"
        );
    }

    #[test]
    fn a_target_with_conflicting_context_and_lineage_owners_is_closed() {
        let context_owner = watched("CONTEXT-ROOT");
        let lineage_owner = watched("LINEAGE-ROOT");
        let shared = shared_with(&context_owner, "OTHER");
        let context = BrowserContextId::new("OWNED-CONTEXT");
        {
            let mut registry = lock(&shared.registry);
            registry.register_watched(lineage_owner.root.clone(), Arc::clone(&lineage_owner));
            registry.register_context(context.clone(), Some(&context_owner.config.ssrf));
            registry
                .contexts
                .get_mut(&context)
                .expect("the context must be registered")
                .owner = Some(Arc::clone(&context_owner));
        }

        let close = adopt_target(
            &shared,
            &target_created_in_context("CONFLICT", Some("LINEAGE-ROOT"), &context).target_info,
        );
        let registry = lock(&shared.registry);

        assert_eq!(close, Some(TargetId::new("CONFLICT")));
        assert!(
            registry.owner_of_target("CONFLICT").is_none(),
            "a conflicting target must not inherit either policy"
        );
        assert!(
            !registry.others.contains(&TargetId::new("CONFLICT")),
            "a conflicting target must not become an unrestricted external target"
        );
        assert!(
            !child_policy_acknowledged(&registry, &TargetId::new("CONFLICT")),
            "a conflicting child must stay paused"
        );
    }

    #[test]
    fn structural_targets_keep_their_policy_without_becoming_popup_teardown_targets() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let context = BrowserContextId::new("OWNED-CONTEXT");
        {
            let mut registry = lock(&shared.registry);
            registry.register_context(context.clone(), Some(&page.config.ssrf));
            registry.opened.insert(page.root.clone(), Some(context.clone()));
        }
        for (id, kind) in [("TAB", "tab"), ("FRAME", "iframe"), ("POPUP", "page")] {
            let mut event = target_created_in_context(id, None, &context);
            event.target_info.r#type = kind.to_owned();
            assert_eq!(adopt_target(&shared, &event.target_info), None);
        }
        let mut registry = lock(&shared.registry);
        install_watch(&mut registry, &page).expect("install context policy");
        for id in ["TAB", "FRAME", "POPUP"] {
            let target = TargetId::new(id);
            assert!(Arc::ptr_eq(&registry.owner_of_target(id).expect("policy owner"), &page));
            assert_eq!(registry.context_targets.get(&target), Some(&context));
            assert!(registry.target_generations.contains_key(&target));
        }
        let mut closed_targets = registry
            .targets
            .iter()
            .map(|(id, _)| id.inner().as_str())
            .collect::<Vec<_>>();
        closed_targets.sort_unstable();
        assert_eq!(closed_targets, ["POPUP", "ROOT"]);
        registry.remove_target(&TargetId::new("TAB"));
        assert!(!registry.structural_targets.contains(&TargetId::new("TAB")));
    }

    #[test]
    fn a_target_created_before_watch_installation_is_bound_by_its_context() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let context = BrowserContextId::new("OWNED-CONTEXT");
        {
            let mut registry = lock(&shared.registry);
            registry.register_context(context.clone(), Some(&page.config.ssrf));
            registry.opened.insert(page.root.clone(), Some(context.clone()));
        }

        let close = adopt_target(&shared, &target_created_in_context("EARLY", None, &context).target_info);
        {
            let registry = lock(&shared.registry);
            assert_eq!(close, None);
            assert_eq!(registry.context_targets.get(&TargetId::new("EARLY")), Some(&context));
            assert!(
                registry.owner_of_target("EARLY").is_none(),
                "the target must remain pending until its context's watch arrives"
            );
            assert!(
                !child_policy_acknowledged(&registry, &TargetId::new("EARLY")),
                "a pending child must stay paused before policy acknowledgement"
            );
            assert!(
                !registry.others.contains(&TargetId::new("EARLY")),
                "a pending owned target must remain fail-closed"
            );
        }

        install_watch(&mut lock(&shared.registry), &page).expect("the matching context policy must install");
        let registry = lock(&shared.registry);
        let owner = registry
            .owner_of_target("EARLY")
            .expect("the pending target must be adopted");
        assert!(
            Arc::ptr_eq(&owner, &page),
            "the context's watch must own the pending target"
        );
        assert!(
            child_policy_acknowledged(&registry, &TargetId::new("EARLY")),
            "the child may resume only after the exact context policy is installed"
        );
    }

    #[tokio::test]
    async fn cancelling_a_page_open_releases_the_opening_guard() {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let task_shared = Arc::clone(&shared);
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let opening = tokio::spawn(async move {
            let _opening = Opening::begin(&task_shared).expect("the firewall is running");
            let _ = started.send(());
            futures::future::pending::<()>().await;
        });
        started_rx.await.expect("the opening task must acquire its guard");
        assert_eq!(shared.opening.load(Ordering::Acquire), 1);

        opening.abort();
        let error = opening.await.expect_err("the opening task must be cancelled");

        assert!(error.is_cancelled(), "the task must end because it was cancelled");
        assert_eq!(shared.opening.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_frame_cached_before_install_cannot_keep_the_old_watch() {
        let old = watched("ROOT");
        let new = watched("ROOT");
        let mut registry = Registry::default();
        registry.register_watched(old.root.clone(), Arc::clone(&old));
        let cached = registry
            .current_frame_owner(old.root.clone(), Some(Arc::clone(&old)))
            .expect("the old target has a generation");
        registry.frames.insert(FrameId::new("CHILD"), cached);

        install_watch(&mut registry, &new).expect("install watch");

        assert!(
            registry.owner_of_frame(&FrameId::new("CHILD")).is_none(),
            "installing a watch must invalidate every frame cached from the preceding generation"
        );
    }

    #[test]
    fn a_frame_cached_as_other_cannot_bypass_a_promoted_watch() {
        let root = TargetId::new("ROOT");
        let mut registry = Registry::default();
        registry.register_other(root.clone());
        let cached = registry
            .current_frame_owner(root, None)
            .expect("the external target has a generation");
        registry.frames.insert(FrameId::new("CHILD"), cached);
        assert!(
            matches!(registry.owner_of_frame(&FrameId::new("CHILD")), Some(Owner::Other)),
            "the negative control must begin as another client's frame"
        );

        let watched = watched("ROOT");
        install_watch(&mut registry, &watched).expect("install watch");
        registry.register_other(TargetId::new("ROOT"));

        assert!(
            registry.owner_of_frame(&FrameId::new("CHILD")).is_none(),
            "promotion to watched must invalidate the unfiltered Other cache entry"
        );
        assert!(
            registry.owner_of_target("ROOT").is_some(),
            "a delayed creation event must not demote the installed watch"
        );
    }

    #[test]
    fn a_destroyed_target_id_cannot_reuse_its_frame_cache() {
        let old = watched("ROOT");
        let mut registry = Registry::default();
        registry.register_watched(old.root.clone(), Arc::clone(&old));
        let cached = registry
            .current_frame_owner(old.root.clone(), Some(old))
            .expect("the original target has a generation");
        registry.frames.insert(FrameId::new("CHILD"), cached);

        registry.remove_target(&TargetId::new("ROOT"));
        let reused = watched("ROOT");
        registry.register_watched(reused.root.clone(), reused);

        assert!(
            registry.owner_of_frame(&FrameId::new("CHILD")).is_none(),
            "a reused target id must not revive a frame cached by the destroyed generation"
        );
    }

    #[test]
    fn a_created_event_processed_after_destruction_cannot_resurrect_a_ghost() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "GHOST");
        let ghost = TargetId::new("GHOST");
        {
            let mut registry = lock(&shared.registry);
            assert!(reconcile_destroyed(&mut registry, &ghost, false));
        }
        let close = reconcile_created(&shared, None);
        let registry = lock(&shared.registry);

        assert!(close.is_none(), "an absent target has nothing to close");
        assert!(
            registry.owner_of_target("GHOST").is_none()
                && !registry.others.contains(&ghost)
                && !registry.target_generations.contains_key(&ghost),
            "a late created event must not resurrect a target Chrome no longer lists"
        );
    }

    #[test]
    fn a_late_destroyed_event_cannot_remove_a_reused_generation() {
        let old = watched("ROOT");
        let new = watched("ROOT");
        let mut registry = Registry::default();
        registry.register_watched(old.root.clone(), old);
        install_watch(&mut registry, &new).expect("install watch");
        let generation = Arc::clone(
            registry
                .target_generations
                .get(&new.root)
                .expect("the reused target has a generation"),
        );

        let removed = reconcile_destroyed(&mut registry, &new.root, true);

        assert!(!removed, "a target Chrome still lists must not be removed");
        assert!(
            registry
                .owner_of_target(new.root.inner())
                .is_some_and(|owner| Arc::ptr_eq(&owner, &new)),
            "the reused target must keep its new owner"
        );
        assert!(
            registry
                .target_generations
                .get(&new.root)
                .is_some_and(|current| Arc::ptr_eq(current, &generation)),
            "the late destroy must not advance or remove the reused generation"
        );
    }

    #[test]
    fn a_delayed_created_event_cannot_demote_an_installed_watch() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let generation = {
            let registry = lock(&shared.registry);
            Arc::clone(
                registry
                    .target_generations
                    .get(&page.root)
                    .expect("the watched root has a generation"),
            )
        };

        let close = adopt_target(&shared, &target_created("ROOT", "page", None, None, None).target_info);
        let registry = lock(&shared.registry);

        assert!(close.is_none(), "the root's creation event is not a popup to close");
        assert!(
            !registry.others.contains(&page.root),
            "the watched root must not become Other"
        );
        assert!(
            registry
                .target_generations
                .get(&page.root)
                .is_some_and(|current| Arc::ptr_eq(current, &generation)),
            "a delayed creation event must leave the installed generation unchanged"
        );
    }

    /// A frame target and a popup of another client's page stay that client's: neither is adopted.
    #[test]
    fn a_frame_and_a_popup_of_another_client_s_page_stay_that_client_s() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let frame = adopt_target(
            &shared,
            &target_created("OTHER-FRAME", "iframe", None, None, Some("OTHER")).target_info,
        );
        let popup = adopt_target(
            &shared,
            &target_created("OTHER-POPUP", "page", Some("OTHER"), Some("OTHER-FRAME"), None).target_info,
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

    /// A paused main-frame document request for `url`, a redirect hop when `redirected` is set.
    fn main_frame_request(url: &str, redirected: bool) -> EventRequestPaused {
        let mut event = serde_json::json!({
            "requestId": format!("interception-{url}"),
            "request": {
                "url": url,
                "method": "GET",
                "headers": {},
                "initialPriority": "VeryHigh",
                "referrerPolicy": "no-referrer",
            },
            "frameId": "MAIN",
            "resourceType": "Document",
        });
        if redirected {
            event["redirectedRequestId"] = serde_json::json!("interception-earlier");
        }
        serde_json::from_value(event).expect("a paused request event")
    }

    #[test]
    fn a_navigation_after_the_first_counts_one_redirect_and_a_redirect_hop_none() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        let verdict = |url: &str, redirected: bool| {
            navigation_verdict(&main_frame_request(url, redirected), &main_frame, 1, &state)
        };
        assert!(
            verdict("http://example.com/", false),
            "the requested navigation is free"
        );
        assert!(
            verdict("http://example.com/hop", true),
            "a redirect hop counts at its response"
        );
        assert!(
            verdict("http://example.com/refresh", false),
            "the first page navigation is within 1"
        );
        assert!(
            !verdict("http://example.com/again", false),
            "the second is past the limit"
        );
        state.lock().expect("state lock").navigation_ended = true;
        assert!(
            verdict("http://example.com/click", false),
            "an ended navigation counts nothing"
        );
        assert_eq!(state.into_inner().expect("state lock").redirects_followed, 1);
    }

    #[test]
    fn a_redirect_past_the_limit_stops_the_requested_navigation_and_drops_a_later_one() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        let redirect = |network_id: &str| {
            let mut event = main_frame_response(network_id, 302);
            event.response_headers = Some(vec![super::HeaderEntry::new("Location", "/next")]);
            event
        };
        assert!(matches!(
            main_frame_verdict(&redirect("A"), &main_frame, 1, &state),
            Verdict::Continue(None)
        ));
        assert!(matches!(
            main_frame_verdict(&redirect("B"), &main_frame, 1, &state),
            Verdict::RenderRedirect
        ));
        assert_eq!(
            state
                .lock()
                .expect("state lock")
                .stopped_response
                .as_ref()
                .map(|stop| stop.status),
            Some(302),
            "the requested navigation stops on the redirect at the limit"
        );
        assert!(matches!(
            main_frame_verdict(&main_frame_response("C", 200), &main_frame, 1, &state),
            Verdict::Continue(None)
        ));
        assert!(
            matches!(
                main_frame_verdict(&redirect("D"), &main_frame, 1, &state),
                Verdict::Abort
            ),
            "a redirect past the limit after the first document is dropped"
        );
    }

    #[test]
    fn a_redirect_to_a_non_web_address_stops_on_the_redirect_response() {
        let main_frame = FrameId::new("MAIN");
        for target in [
            "mailto:someone@example.com",
            "data:text/html,hi",
            "file:///etc/hostname",
            "myapp://open",
        ] {
            let state = Mutex::new(InterceptOutcome::default());
            let mut event = main_frame_redirect("SEED", target);
            event
                .response_headers
                .as_mut()
                .expect("redirect headers")
                .push(super::HeaderEntry::new("X-Redirect-Marker", "kept"));
            let Verdict::RenderRedirect = main_frame_verdict(&event, &main_frame, 10, &state) else {
                panic!("the terminal redirect must be rendered without being followed");
            };
            let state = state.into_inner().expect("state lock");
            let stopped = state.stopped_response.expect("the redirect response must be kept");
            assert_eq!(
                (stopped.status, stopped.url.as_str()),
                (302, event.request.url.as_str())
            );
            assert_eq!(stopped.headers["location"], [target]);
            assert_eq!(stopped.headers["x-redirect-marker"], ["kept"]);
            assert!(stopped.terminal_intercepted);
            assert_eq!(state.redirects_followed, 0, "{target} is not followed");
        }
    }

    #[test]
    fn a_seed_without_a_document_is_ready_only_after_its_inert_replacement_is_delivered() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        let event = main_frame_response("NO_DOCUMENT", 204);
        assert!(matches!(
            main_frame_verdict(&event, &main_frame, 10, &state),
            Verdict::Stop
        ));
        assert!(
            !state
                .lock()
                .expect("state lock")
                .stopped_response
                .as_ref()
                .expect("stopped response")
                .ready
        );
        assert!(
            !state
                .lock()
                .expect("state lock")
                .stopped_response
                .as_ref()
                .expect("stopped response")
                .terminal_intercepted
        );

        complete_stopped_response_outcome(&state, &event.request_id, None);
        assert!(
            state
                .into_inner()
                .expect("state lock")
                .stopped_response
                .expect("stopped response")
                .ready
        );
    }

    #[test]
    fn an_older_response_completion_cannot_complete_its_replacement() {
        let old = FetchRequestId::new("OLD");
        let new = FetchRequestId::new("NEW");
        let outcome = Mutex::new(InterceptOutcome {
            stopped_response: Some(super::StoppedResponse {
                url: "https://example.com/new".to_owned(),
                status: 302,
                headers: std::collections::HashMap::new(),
                body: String::new(),
                body_bytes: Vec::new(),
                request_id: Some(new.clone()),
                terminal_intercepted: true,
                ready: false,
            }),
            ..InterceptOutcome::default()
        });

        complete_stopped_response_outcome(&outcome, &old, Some(b"old".to_vec()));
        {
            let outcome = outcome.lock().expect("outcome lock");
            let stopped = outcome.stopped_response.as_ref().expect("new response");
            assert!(!stopped.ready);
            assert!(stopped.body_bytes.is_empty());
        }

        complete_stopped_response_outcome(&outcome, &new, Some(b"new".to_vec()));
        let outcome = outcome.into_inner().expect("outcome lock");
        let stopped = outcome.stopped_response.expect("new response");
        assert!(stopped.ready);
        assert_eq!(stopped.body_bytes, b"new");
    }

    #[test]
    fn a_later_normal_response_clears_an_older_stopped_response() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        let terminal = main_frame_redirect("TERMINAL", "mailto:someone@example.com");
        assert!(matches!(
            main_frame_verdict(&terminal, &main_frame, 10, &state),
            Verdict::RenderRedirect
        ));

        let normal = main_frame_response("NORMAL", 200);
        assert!(matches!(
            main_frame_verdict(&normal, &main_frame, 10, &state),
            Verdict::Continue(None)
        ));
        complete_stopped_response_outcome(&state, &terminal.request_id, Some(b"stale".to_vec()));
        assert!(state.into_inner().expect("state lock").stopped_response.is_none());
    }

    #[test]
    fn a_later_response_without_a_document_is_dropped() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        assert!(matches!(
            main_frame_verdict(&main_frame_response("START", 200), &main_frame, 1, &state),
            Verdict::Continue(None)
        ));
        assert!(matches!(
            main_frame_verdict(&main_frame_response("LATE", 204), &main_frame, 1, &state),
            Verdict::Abort
        ));
        assert!(
            state.lock().expect("state lock").stopped_response.is_none(),
            "the original document stays the page"
        );
    }

    #[test]
    fn keeps_only_the_committed_document_and_the_newest_response() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        for (network_id, status, committed, dropped) in [
            ("A", 200, None, false),
            ("B", 204, Some("A"), true),
            ("C", 204, Some("A"), true),
        ] {
            state.lock().expect("state lock").committed_loader = committed.map(str::to_owned);
            let verdict = main_frame_verdict(&main_frame_response(network_id, status), &main_frame, 0, &state);
            assert_eq!(matches!(verdict, Verdict::Abort), dropped, "{network_id}");
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

    #[test]
    fn an_error_page_commit_is_paired_with_its_recorded_success_response() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        assert!(matches!(
            main_frame_verdict(&main_frame_response("BROKEN", 200), &main_frame, 0, &state),
            Verdict::Continue(None)
        ));
        record_main_frame_commit(&state, "BROKEN", Some("http://example.com/broken"));

        let (failed_url, response) = failed_document_response(&state.lock().expect("state lock"))
            .expect("the error-page commit must retain its matching response");
        assert_eq!(failed_url, "http://example.com/broken");
        assert_eq!(response.status, 200);
    }

    #[test]
    fn a_normal_commit_clears_the_recorded_error_page() {
        let state = Mutex::new(InterceptOutcome::default());
        record_main_frame_commit(&state, "BROKEN", Some("http://example.com/broken"));
        record_main_frame_commit(&state, "RECOVERED", None);

        let state = state.into_inner().expect("state lock");
        assert_eq!(state.committed_loader.as_deref(), Some("RECOVERED"));
        assert_eq!(state.committed_unreachable_url, None);
        assert!(failed_document_response(&state).is_none());
    }

    #[test]
    fn a_failed_document_load_is_paired_with_its_recorded_success_response() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        assert!(matches!(
            main_frame_verdict(&main_frame_response("BROKEN", 200), &main_frame, 0, &state),
            Verdict::Continue(None)
        ));
        assert!(record_document_failure_outcome(
            &mut state.lock().expect("state lock"),
            "BROKEN",
            false,
            "net::ERR_CONTENT_DECODING_FAILED"
        ));

        let (failed_url, response) = failed_document_response(&state.lock().expect("state lock"))
            .expect("the load failure must retain its matching response");
        assert_eq!(failed_url, "http://example.com/BROKEN");
        assert_eq!(response.status, 200);
    }

    #[test]
    fn a_later_normal_response_clears_the_failed_document_load() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        assert!(matches!(
            main_frame_verdict(&main_frame_response("BROKEN", 200), &main_frame, 0, &state),
            Verdict::Continue(None)
        ));
        assert!(record_document_failure_outcome(
            &mut state.lock().expect("state lock"),
            "BROKEN",
            false,
            "net::ERR_CONTENT_DECODING_FAILED"
        ));
        assert!(matches!(
            main_frame_verdict(&main_frame_response("RECOVERED", 200), &main_frame, 0, &state),
            Verdict::Continue(None)
        ));

        let state = state.into_inner().expect("state lock");
        assert_eq!(state.failed_document, None);
        assert!(failed_document_response(&state).is_none());
    }

    #[test]
    fn a_delayed_failure_from_an_older_document_is_ignored() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        for request_id in ["A", "B"] {
            assert!(matches!(
                main_frame_verdict(&main_frame_response(request_id, 200), &main_frame, 0, &state),
                Verdict::Continue(None)
            ));
            record_main_frame_commit(&state, request_id, None);
        }

        assert!(!record_document_failure_outcome(
            &mut state.lock().expect("state lock"),
            "A",
            false,
            "net::ERR_CONTENT_DECODING_FAILED"
        ));
        let state = state.into_inner().expect("state lock");
        assert_eq!(state.latest_document_request.as_deref(), Some("B"));
        assert_eq!(state.failed_document, None);
        assert!(failed_document_response(&state).is_none());
    }

    #[test]
    fn a_no_document_response_is_not_recorded_as_an_unreadable_success() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        assert!(matches!(
            main_frame_verdict(&main_frame_response("NO-CONTENT", 204), &main_frame, 0, &state),
            Verdict::Stop
        ));
        assert!(!record_document_failure_outcome(
            &mut state.lock().expect("state lock"),
            "NO-CONTENT",
            false,
            "net::ERR_FAILED"
        ));
        assert!(failed_document_response(&state.into_inner().expect("state lock")).is_none());
    }

    #[test]
    fn a_canceled_success_response_is_not_recorded_as_an_unreadable_document() {
        let main_frame = FrameId::new("MAIN");
        for (canceled, error_text) in [(true, "net::ERR_FAILED"), (false, "net::ERR_ABORTED")] {
            let state = Mutex::new(InterceptOutcome::default());
            assert!(matches!(
                main_frame_verdict(&main_frame_response("SUPERSEDED", 200), &main_frame, 0, &state),
                Verdict::Continue(None)
            ));
            assert!(!record_document_failure_outcome(
                &mut state.lock().expect("state lock"),
                "SUPERSEDED",
                canceled,
                error_text
            ));
            assert!(failed_document_response(&state.into_inner().expect("state lock")).is_none());
        }
    }

    #[test]
    fn a_refused_url_is_found_in_the_form_the_check_lists_it() {
        let refused = vec!["http://10.0.0.1/secret".to_owned()];
        for url in [
            "http://10.0.0.1/secret",
            "http://user:pw@10.0.0.1/secret",
            "http://10.0.0.1/secret#part",
        ] {
            assert_eq!(
                super::listed_refusal(url, &refused).as_deref(),
                Some("http://10.0.0.1/secret"),
                "{url}"
            );
        }
        for url in ["http://10.0.0.1/other", "http://10.0.0.2/secret", "not a url"] {
            assert_eq!(super::listed_refusal(url, &refused), None, "{url}");
        }
    }

    /// A paused main-frame redirect from the navigation `network_id` to `location`.
    fn main_frame_redirect(network_id: &str, location: &str) -> EventRequestPaused {
        let mut event = main_frame_response(network_id, 302);
        event.response_headers = Some(vec![HeaderEntry {
            name: "Location".to_owned(),
            value: location.to_owned(),
        }]);
        event
    }

    #[test]
    fn a_document_carries_the_redirects_of_its_own_navigation() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        let events = [
            main_frame_redirect("SEED", "/start"),
            main_frame_response("SEED", 200),
            main_frame_redirect("LATE", "/next"),
            main_frame_redirect("LATE", "/dl"),
            main_frame_response("LATE", 404),
            main_frame_redirect("CANCELLED", "/elsewhere"),
            main_frame_response("PLAIN", 404),
        ];
        for event in &events {
            state.lock().expect("state lock").committed_loader = None;
            assert!(matches!(
                main_frame_verdict(event, &main_frame, 5, &state),
                Verdict::Continue(None)
            ));
        }
        let state = state.into_inner().expect("state lock");
        let redirects = |id: &str| state.documents[id].redirects;
        assert_eq!(
            (redirects("SEED"), redirects("LATE"), redirects("PLAIN")),
            (1, 2, 0),
            "each document counts only the redirects of its own navigation"
        );
        assert_eq!(
            state.redirects_followed, 4,
            "the limit counts every redirect of the main frame, apart from each document's own count"
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

    /// The interception outcome keeps each main-frame response's headers; its `Debug` must hide
    /// a session cookie the server sets and keep the other header values.
    #[test]
    fn intercept_outcome_debug_hides_a_response_set_cookie() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        let headers = std::collections::HashMap::from([
            ("set-cookie".to_owned(), vec![format!("sid={SECRET}; HttpOnly")]),
            ("content-type".to_owned(), vec!["text/html".to_owned()]),
        ]);
        let outcome = InterceptOutcome {
            stopped_response: Some(super::StoppedResponse {
                url: format!("https://user:{SECRET}@example.com/"),
                status: 204,
                headers: headers.clone(),
                body: SECRET.to_owned(),
                body_bytes: SECRET.as_bytes().to_vec(),
                request_id: None,
                terminal_intercepted: false,
                ready: true,
            }),
            documents: std::collections::HashMap::from([(
                "loader-1".to_owned(),
                super::DocumentResponse {
                    url: "https://example.com/".to_owned(),
                    status: 200,
                    headers,
                    redirects: 0,
                },
            )]),
            ..InterceptOutcome::default()
        };
        for rendered in [format!("{outcome:?}"), format!("{outcome:#?}")] {
            assert!(!rendered.contains(SECRET), "a secret printed: {rendered}");
            assert_eq!(rendered.matches("text/html").count(), 2, "{rendered}");
            assert_eq!(rendered.matches("set-cookie").count(), 2, "{rendered}");
        }
    }
}

#[cfg(test)]
mod race_tests {
    //! Races the listener closes by construction, made deterministic with injected delays.
    //! These launch a real Chrome and skip when none is found.
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use chromiumoxide::Browser;
    use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
    use chromiumoxide::cdp::browser_protocol::fetch::DisableParams as FetchDisableParams;
    use chromiumoxide::cdp::browser_protocol::network::CookieParam;
    use chromiumoxide::cdp::browser_protocol::storage::{
        GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
    };
    use chromiumoxide::cdp::browser_protocol::target::{GetBrowserContextsParams, GetTargetsParams, TargetId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_stream::StreamExt;

    use super::{
        ACTION_GRACE, ACTION_SETTLE_LIMIT, BrowserFirewall, BrowserOrigin, CALL_SITE_DELAYS, EventTargetCreated,
        EventTargetDestroyed, FrameId, Owner, PageContext, TestDelays, UrlGate, lock,
    };

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
        let mut builder = chromiumoxide::browser::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .manage_child_targets(false)
            .user_data_dir(dir);
        if let Some(chrome) = crate::browser_pool::chrome_executable(None) {
            builder = builder.chrome_executable(chrome);
        }
        let config = crate::browser_pool::tests::expect_chrome_or_skip(
            test_name,
            crate::browser_pool::apply_default_args(builder, &[]).build(),
        )?;
        let (browser, handler) =
            crate::browser_pool::tests::expect_chrome_or_skip(test_name, Browser::launch(config).await)?;
        crate::browser_pool::spawn_handler(handler);
        Some(Arc::new(browser))
    }

    /// A command sent to a Chrome that has died ends with an error. The test's CDP handler stops
    /// at the broken websocket, which fails every command still waiting on it; a handler loop that
    /// kept polling held them, and the test, until the CI job limit (xberg-io/crawlberg#573).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_command_to_a_chrome_that_died_ends_with_an_error() {
        let test_name = "a_command_to_a_chrome_that_died_ends_with_an_error";
        let Some(mut browser) = launch(test_name).await else {
            return;
        };
        let chrome = Arc::get_mut(&mut browser).expect("the test holds the only reference to its browser");
        let killed = chrome.kill().await;
        assert!(
            matches!(killed, Some(Ok(()))),
            "the launched Chrome must be killed: {killed:?}"
        );
        let reply = tokio::time::timeout(Duration::from_secs(20), browser.execute(GetTargetsParams::default())).await;
        assert!(
            matches!(reply, Ok(Err(_))),
            "a command to a Chrome that died must end with an error within 20 s, got {}",
            match reply {
                Ok(Ok(_)) => "a reply",
                Ok(Err(_)) => "an error",
                Err(_) => "no answer",
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn checking_a_target_on_a_dead_chrome_returns_an_error() {
        let test_name = "checking_a_target_on_a_dead_chrome_returns_an_error";
        let Some(mut browser) = launch(test_name).await else {
            return;
        };
        let chrome = Arc::get_mut(&mut browser).expect("the test holds the only reference to its browser");
        let killed = chrome.kill().await;
        assert!(
            matches!(killed, Some(Ok(()))),
            "the launched Chrome must be killed: {killed:?}"
        );

        let missing = TargetId::new("missing");
        let checked = target_is_open(&browser, &missing);
        let reply = tokio::time::timeout(Duration::from_secs(20), checked).await;
        assert!(
            matches!(reply, Ok(Err(_))),
            "checking a target on a dead Chrome must return an error within 20 s"
        );
    }

    /// Allow `localhost`, where the test pages are served, and refuse the loopback address.
    fn config() -> crate::types::CrawlConfig {
        crate::types::CrawlConfig::builder()
            .allow_private_networks(false)
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
    ) -> chromiumoxide::error::Result<bool> {
        browser.execute(GetTargetsParams::default()).await.map(|response| {
            response
                .result
                .target_infos
                .iter()
                .any(|info| info.target_id == *target)
        })
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

    /// On a browser that is killed when done, a tab still sending after the check stopped stays
    /// refused: interception stays on with nothing answering, so its requests wait paused until
    /// the browser is gone. Turning interception off afterwards lets them out, which shows the tab
    /// was sending all along.
    ///
    /// ~keep The tab outside the check stands in for any target still alive when the session
    /// ~keep ends, as a page or popup Chrome has not destroyed yet under load
    /// ~keep (xberg-io/crawlberg#468): the check's own pages go with their contexts at the stop.
    /// ~keep The tab is opened after the check starts and stays on about:blank, as in
    /// ~keep `a_tab_outside_the_check_reaches_the_network_after_the_check_is_stopped`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_killed_browser_keeps_refusing_a_tab_still_sending_after_the_check_stops() {
        let test_name = "a_killed_browser_keeps_refusing_a_tab_still_sending_after_the_check_stops";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start(Arc::clone(&browser), BrowserOrigin::Killed, PageContext::Isolated)
            .await
            .expect("the listener must start");
        let other = browser.new_page("about:blank").await.expect("page");
        let (denied, denied_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!(
                "setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 50); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reached_while_on = denied_hits.load(Ordering::SeqCst);
        firewall.stop().await;
        let reached_before_kill = served(&denied_hits).await;

        let mut browser = Arc::into_inner(browser).expect("the stopped check lets go of the browser");
        let _ = browser.execute(FetchDisableParams::default()).await;
        let reached_once_off = served(&denied_hits).await;
        drop(other);
        let _ = browser.kill().await;

        assert_eq!(
            reached_while_on, 0,
            "{test_name}: the tab's requests must be refused while the check runs"
        );
        assert!(
            !reached_before_kill,
            "{test_name}: a tab still sending after the check stopped before a kill must not reach \
             the denied address, got {} requests",
            denied_hits.load(Ordering::SeqCst)
        );
        assert!(
            reached_once_off,
            "{test_name}: the tab must reach the denied address once interception is off, or it \
             was never sending and the second assertion proves nothing"
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

    /// On an external browser, a session page sending when its watch closes it reaches nothing,
    /// before the stop or after it, and is gone when the stop returns. The browser, and a tab
    /// another client had open before the check started, keep working.
    ///
    /// ~keep The page's context goes at the close, and with it every request of the page Chrome
    /// ~keep still holds, before the stop turns interception off.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_external_browser_keeps_refusing_a_session_page_until_chrome_destroys_it() {
        let test_name = "an_external_browser_keeps_refusing_a_session_page_until_chrome_destroys_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let other = browser.new_page("about:blank").await.expect("the other client's tab");
        open_blank_site(&other).await;
        let run = session_page_end(&browser, false).await;
        let (reachable, other_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!("fetch({reachable:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let other_works = served(&other_hits).await;
        let mut browser = Arc::into_inner(browser).expect("the stopped check lets go of the browser");
        let _ = browser.kill().await;

        assert!(
            run.refused,
            "{test_name}: the watched page's requests must be refused while it is watched"
        );
        assert!(
            !run.reached,
            "{test_name}: a session page sending when its watch closes it must not reach the denied \
             address, got {} requests",
            run.final_hits
        );
        assert!(
            !run.open_after_stop,
            "{test_name}: the session page must be gone when the stop returns"
        );
        assert!(
            other_works,
            "{test_name}: the other client's tab must still reach the network after the stop"
        );
    }

    /// On an external browser, a session page sending when a stop arrives as its watch closes it
    /// reaches nothing, before the stop or after it, and is gone when the stop returns.
    ///
    /// ~keep The stop is sent right after the close, so it takes the watch's end into its drain
    /// ~keep before the end has disposed the page's context (xberg-io/crawlberg#484).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_external_browser_keeps_refusing_a_session_page_stopped_as_its_watch_closes() {
        let test_name = "an_external_browser_keeps_refusing_a_session_page_stopped_as_its_watch_closes";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let run = session_page_end(&browser, true).await;
        let mut browser = Arc::into_inner(browser).expect("the stopped check lets go of the browser");
        let _ = browser.kill().await;

        assert!(
            run.refused,
            "{test_name}: the watched page's requests must be refused while it is watched"
        );
        assert!(
            !run.reached,
            "{test_name}: a session page sending when the check stops must not reach the denied \
             address, got {} requests",
            run.final_hits
        );
        assert!(
            !run.open_after_stop,
            "{test_name}: the session page must be gone when the stop returns"
        );
    }

    /// What `session_page_end` saw of the session page.
    struct SessionPageEnd {
        refused: bool,
        open_after_stop: bool,
        reached: bool,
        final_hits: usize,
    }

    /// On an external browser, start a check, open a session page with it that sends to a denied
    /// address every 10 ms, close its watch and stop the check. With `stop_as_closing` set, the
    /// stop is sent while the close is still running, and the page is not looked up before it.
    async fn session_page_end(browser: &Arc<Browser>, stop_as_closing: bool) -> SessionPageEnd {
        let firewall = BrowserFirewall::start(Arc::clone(browser), BrowserOrigin::External, PageContext::Copied)
            .await
            .expect("the listener must start");
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let session_target = page.target_id().clone();
        let allowing = crate::types::CrawlConfig::builder()
            .allow_private_networks(false)
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("a.localhost"))
            .build();
        let watch = firewall
            .handle()
            .watch(&page, &allowing, 0)
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
        if stop_as_closing {
            tokio::join!(watch.close(), firewall.stop());
        } else {
            watch.close().await;
            firewall.stop().await;
        }
        let open_after_stop = open_targets(browser).await.contains(&session_target);
        let reached = served(&denied_hits).await;
        drop(page);
        SessionPageEnd {
            refused,
            open_after_stop,
            reached,
            final_hits: denied_hits.load(Ordering::SeqCst),
        }
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

    /// On an external browser, a stop that arrives while a page is being parked returns once the
    /// park has ended, and the park closes the page's popup.
    ///
    /// ~keep The injected close delay slows the park while it closes the page's popup, so the stop
    /// ~keep finds the page still registered and ending (xberg-io/crawlberg#484).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_during_a_park_returns_once_the_park_has_ended_on_an_external_browser() {
        let test_name = "a_stop_during_a_park_returns_once_the_park_has_ended_on_an_external_browser";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            close: Duration::from_millis(1500),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::External,
            PageContext::Copied,
            delays,
        )
        .await
        .expect("the listener must start");
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let root = page.target_id().clone();
        let watch = firewall
            .handle()
            .watch(&page, &config(), 0)
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
    }

    /// Lifecycle queries run outside the listener loop: holding the oldest one cannot block a
    /// watch command, a paused request, or Stop, and Stop discards it without later mutation. ~keep
    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_lifecycle_query_does_not_block_requests_commands_or_stop() {
        let test_name = "a_held_lifecycle_query_does_not_block_requests_commands_or_stop";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = closed_match_gate();
        let delays = TestDelays {
            lifecycle_query_gate: Some(Arc::clone(&gate)),
            bypass_egress: true,
            ..TestDelays::default()
        };
        let browser_for_check = Arc::clone(&browser);
        CALL_SITE_DELAYS
            .scope(delays, async {
                let firewall = BrowserFirewall::start(
                    Arc::clone(&browser_for_check),
                    BrowserOrigin::External,
                    PageContext::Copied,
                )
                .await
                .expect("the listener must start");
                let page = firewall
                    .handle()
                    .new_page(None, None)
                    .await
                    .expect("the check must open a page");
                assert!(
                    wait_held_count(&gate, 1).await,
                    "{test_name}: a lifecycle reconciliation must be held"
                );
                let watch = tokio::time::timeout(Duration::from_secs(10), firewall.handle().watch(&page, &config(), 0))
                    .await
                    .expect("the watch command must remain serviceable")
                    .expect("the watch must start");
                tokio::time::timeout(Duration::from_secs(10), open_blank_site(&page))
                    .await
                    .expect("a paused navigation must be answered while reconciliation is held");
                let (denied, denied_hits) = denied_listener().await;
                let answer: String = tokio::time::timeout(
                    Duration::from_secs(10),
                    page.evaluate(format!(
                        "fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
                    )),
                )
                .await
                .expect("the paused request must be answered")
                .ok()
                .and_then(|result| result.into_value().ok())
                .unwrap_or_default();
                watch.park().await;
                tokio::time::timeout(Duration::from_secs(10), firewall.stop())
                    .await
                    .expect("Stop must discard the held reconciliation and return");
                let held_at_stop = lock(&gate.held).len();
                tokio::task::yield_now().await;
                assert_eq!(answer, "refused", "{test_name}: the watched request must be refused");
                assert_eq!(
                    denied_hits.load(Ordering::SeqCst),
                    0,
                    "{test_name}: the held reconciliation must not bypass the watched policy"
                );
                assert_eq!(
                    lock(&gate.held).len(),
                    held_at_stop,
                    "{test_name}: no reconciliation may resume or start after Stop returns"
                );
                drop(page);
            })
            .await;
        close(browser).await;
    }

    /// Frame-tree lookup crosses CDP awaits, so ownership must be re-read after the lookup before
    /// its result is cached. The handoff gate makes that ownership change deterministic. ~keep
    #[tokio::test(flavor = "multi_thread")]
    async fn frame_attribution_uses_the_policy_installed_after_its_snapshot() {
        let test_name = "frame_attribution_uses_the_policy_installed_after_its_snapshot";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = closed_match_gate();
        let delays = TestDelays {
            attribute_snapshot_gate: Some(Arc::clone(&gate)),
            bypass_egress: true,
            ..TestDelays::default()
        };
        let browser_for_check = Arc::clone(&browser);
        CALL_SITE_DELAYS
            .scope(delays, async {
                let firewall = BrowserFirewall::start(
                    Arc::clone(&browser_for_check),
                    BrowserOrigin::External,
                    PageContext::Copied,
                )
                .await
                .expect("the listener must start");
                let page = firewall
                    .handle()
                    .new_page(None, None)
                    .await
                    .expect("the check must open a page");
                let first = firewall
                    .handle()
                    .watch(&page, &config(), 0)
                    .await
                    .expect("the strict watch must start");
                open_blank_site(&page).await;
                let frame_ready: String = page
                    .evaluate(
                        "new Promise(resolve => { const frame = document.createElement('iframe'); \
                         frame.onload = () => { window.__handoffFrame = frame; resolve('ready'); }; \
                         frame.srcdoc = `<script>addEventListener('message', async event => { \
                         if (event.data.kind !== 'handoff-fetch') return; \
                         const result = await fetch(event.data.url, { mode: 'no-cors' }) \
                         .then(() => 'reached', () => 'refused'); \
                         parent.postMessage({ kind: 'handoff-result', result }, '*'); \
                         });<\\/script>`; document.body.appendChild(frame); })",
                    )
                    .await
                    .ok()
                    .and_then(|result| result.into_value().ok())
                    .unwrap_or_default();
                assert_eq!(frame_ready, "ready", "{test_name}: the child frame must load");
                // ~keep Iframe setup can populate the cache; clear it so the request necessarily
                // ~keep snapshots target ownership and crosses the gated CDP frame-tree lookup.
                lock(&first.shared.registry).frames.clear();

                let (denied, denied_hits) = denied_listener().await;
                let requesting_page = page.clone();
                let requesting = tokio::spawn(async move {
                    requesting_page
                        .evaluate(format!(
                            "new Promise(resolve => {{ const receive = event => {{ \
                             if (event.source !== window.__handoffFrame.contentWindow || \
                             event.data.kind !== 'handoff-result') return; \
                             removeEventListener('message', receive); resolve(event.data.result); }}; \
                             addEventListener('message', receive); \
                             window.__handoffFrame.contentWindow.postMessage( \
                             {{ kind: 'handoff-fetch', url: {denied:?} }}, '*'); }})"
                        ))
                        .await
                        .ok()
                        .and_then(|result| result.into_value().ok())
                        .unwrap_or_default()
                });
                assert!(
                    wait_held_count(&gate, 1).await,
                    "{test_name}: attribution must pause after its ownership snapshot"
                );
                let child_frame = lock(&gate.held)
                    .first()
                    .cloned()
                    .expect("the gate records the child frame");

                first.park().await;
                let permissive = crate::types::CrawlConfig::builder()
                    .allow_private_networks(true)
                    .build();
                let second = firewall
                    .handle()
                    .watch(&page, &permissive, 0)
                    .await
                    .expect("the permissive watch must take ownership");
                gate.permits.add_permits(1);
                let answer: String = tokio::time::timeout(Duration::from_secs(10), requesting)
                    .await
                    .expect("the child request must finish")
                    .expect("the request task must finish");
                let frame_has_new_owner = matches!(
                    lock(&second.shared.registry).owner_of_frame(&FrameId::new(child_frame)),
                    Some(Owner::Watched(owner)) if Arc::ptr_eq(&owner, &second.page)
                );
                let reached = served(&denied_hits).await;

                second.close().await;
                firewall.stop().await;
                drop(page);
                assert_eq!(
                    answer, "reached",
                    "{test_name}: the frame request must use the newly installed permissive policy"
                );
                assert!(
                    frame_has_new_owner,
                    "{test_name}: the frame cache must hold the newly installed watch"
                );
                assert!(reached, "{test_name}: the allowed frame request must reach its server");
            })
            .await;
        close(browser).await;
    }

    /// A parked root remains under its ending policy until the next policy takes ownership.
    /// A popup created in that handoff is therefore closed, never treated as another external
    /// browser client's unrestricted target. ~keep
    #[tokio::test(flavor = "multi_thread")]
    async fn an_external_browser_keeps_a_popup_fail_closed_during_policy_handoff() {
        let test_name = "an_external_browser_keeps_a_popup_fail_closed_during_policy_handoff";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let other = browser.new_page("about:blank").await.expect("the other client's tab");
        open_blank_site(&other).await;
        let gate = Arc::new(UrlGate {
            permits: tokio::sync::Semaphore::new(1),
            held: Mutex::default(),
        });
        let delays = TestDelays {
            watch_handoff_gate: Some(Arc::clone(&gate)),
            bypass_egress: true,
            ..TestDelays::default()
        };
        let browser_for_check = Arc::clone(&browser);
        CALL_SITE_DELAYS
            .scope(delays, async {
                let firewall = BrowserFirewall::start(
                    Arc::clone(&browser_for_check),
                    BrowserOrigin::External,
                    PageContext::Copied,
                )
                .await
                .expect("the listener must start");
                let page = firewall
                    .handle()
                    .new_page(None, None)
                    .await
                    .expect("the check must open a page");
                let root = page.target_id().clone();
                let permissive = crate::types::CrawlConfig::builder()
                    .allow_private_networks(true)
                    .build();
                let first = firewall
                    .handle()
                    .watch(&page, &permissive, 0)
                    .await
                    .expect("the permissive watch must start");
                open_blank_site(&page).await;
                let (denied, denied_hits) = denied_listener().await;
                let control: String = other
                    .evaluate(format!(
                        "fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
                    ))
                    .await
                    .ok()
                    .and_then(|result| result.into_value().ok())
                    .unwrap_or_default();
                assert_eq!(
                    control, "reached",
                    "{test_name}: another client's target must reach the denied server, or the negative control is inert"
                );
                assert!(
                    served(&denied_hits).await,
                    "{test_name}: the denied server must record the positive control"
                );
                let control_hits = denied_hits.load(Ordering::SeqCst);

                first.park().await;
                let handle = firewall.handle();
                let reused = page.clone();
                let strict = config();
                let watching = tokio::spawn(async move { handle.watch(&reused, &strict, 0).await });
                assert!(
                    wait_held_count(&gate, 2).await,
                    "{test_name}: the strict watch must stop at the handoff gate"
                );

                let (parked_denied, parked_hits) = denied_listener().await;
                let parked: String = page
                    .evaluate(format!(
                        "fetch({parked_denied:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
                    ))
                    .await
                    .ok()
                    .and_then(|result| result.into_value().ok())
                    .unwrap_or_default();

                let mut created = browser_for_check
                    .event_listener::<EventTargetCreated>()
                    .await
                    .expect("listen for the popup");
                let mut destroyed = browser_for_check
                    .event_listener::<EventTargetDestroyed>()
                    .await
                    .expect("listen for the popup close");
                // ~keep Chrome 155 never replies to a synchronous window.open when its
                // ~keep debugger-held popup is closed. Observe creation and destruction instead
                // ~keep of waiting for that script while the policy handoff remains gated.
                page.evaluate("setTimeout(() => window.open('about:blank'), 0); 1")
                    .await
                    .expect("schedule the popup");
                let popup = tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(event) = created.next().await {
                        if event.target_info.opener_id.as_ref() == Some(&root) {
                            return Some(event.target_info.target_id.clone());
                        }
                    }
                    None
                })
                .await
                .expect("Chrome must report the popup")
                .expect("the popup event stream must remain open");
                let popup_answer: String = tokio::time::timeout(Duration::from_secs(5), async {
                    let popup_page = browser_for_check.get_page(popup.clone()).await.ok()?;
                    popup_page
                        .evaluate(format!(
                            "fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
                        ))
                        .await
                        .ok()?
                        .into_value()
                        .ok()
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
                let popup_closed = tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(event) = destroyed.next().await {
                        if event.target_id == popup {
                            return true;
                        }
                    }
                    false
                })
                .await
                .unwrap_or(false);
                assert!(
                    popup_closed,
                    "{test_name}: the popup created during handoff must be closed"
                );
                assert_ne!(
                    popup_answer, "reached",
                    "{test_name}: the handoff popup was treated as another client's target"
                );
                assert_eq!(
                    denied_hits.load(Ordering::SeqCst),
                    control_hits,
                    "{test_name}: the handoff popup reached the denied server"
                );
                assert_eq!(
                    parked, "refused",
                    "{test_name}: the parked root must remain refused between watches"
                );
                assert_eq!(
                    parked_hits.load(Ordering::SeqCst),
                    0,
                    "{test_name}: the parked root reached the denied server"
                );

                assert!(!firewall.has_failed(), "the refused popup must not stop either security controller");
                let unaffected: String = other
                    .evaluate(format!(
                        "fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
                    ))
                    .await
                    .expect("the other client's tab must remain responsive")
                    .into_value()
                    .expect("the other client's fetch result must be a string");
                assert_eq!(unaffected, "reached", "the popup refusal must preserve another client's tab");
                gate.permits.add_permits(1);
                let second = watching
                    .await
                    .expect("the watch task must finish")
                    .expect("the strict watch must start");
                second.close().await;
                firewall.stop().await;
                drop(page);
            })
            .await;
        drop(other);
        close(browser).await;
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
    /// the park.
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
        let firewall = BrowserFirewall::start(Arc::clone(&browser), BrowserOrigin::External, PageContext::Copied)
            .await
            .expect("the listener must start");
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let root = page.target_id().clone();
        let allowing = crate::types::CrawlConfig::builder()
            .allow_private_networks(false)
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("a.localhost"))
            .build();
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
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_openerless_popup_inherits_its_browser_context_s_policy() {
        let test_name = "an_openerless_popup_inherits_its_browser_context_s_policy";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start(Arc::clone(&browser), BrowserOrigin::External, PageContext::Isolated)
            .await
            .expect("the listener must start");
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let allowing = crate::types::CrawlConfig::builder()
            .allow_private_networks(false)
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("a.localhost"))
            .build();
        let watch = firewall
            .handle()
            .watch(&page, &allowing, 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let (denied, denied_hits) = denied_listener().await;
        let (popup_url, allowed_hits) = frame_site(format!(
            "<script>fetch({denied:?}, {{ mode: 'no-cors' }}).finally(() => fetch('/ok'));</script>"
        ))
        .await;
        page.evaluate(format!("window.open({popup_url:?}, '_blank', 'noopener'); 1"))
            .await
            .expect("the page must open its popup");
        let popup_executed = served(&allowed_hits).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let denied_count = denied_hits.load(Ordering::SeqCst);
        let controller_failed = firewall.handle().shared.failed.load(Ordering::Acquire);
        let open = browser
            .execute(GetTargetsParams::default())
            .await
            .map(|response| {
                response
                    .result
                    .target_infos
                    .into_iter()
                    .map(|target| (target.r#type, target.url))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;

        assert!(
            popup_executed,
            "{test_name}: the popup must execute through its post-attempt marker; controller failed: \
             {controller_failed}; open targets: {open:?}; denied requests: {denied_count}"
        );
        assert_eq!(
            denied_count, 0,
            "{test_name}: the denied endpoint must receive no openerless-popup request"
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

    /// How a test reads the refusal of a request to a denied address a watched page sends.
    enum Read {
        /// The action's refusal.
        Action,
        /// The page's refused URLs.
        RefusedUrls,
    }

    /// A match gate with no permit: each request it holds waits for a permit of its own.
    fn closed_match_gate() -> Arc<UrlGate> {
        Arc::new(UrlGate {
            permits: tokio::sync::Semaphore::new(0),
            held: std::sync::Mutex::default(),
        })
    }

    /// Wait up to ten seconds until `gate` holds a request to `url`; `false` if it never did.
    async fn wait_held(gate: &UrlGate, url: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::lock(&gate.held).iter().any(|held| held == url) {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        true
    }

    /// Wait until `gate` has held `count` watches; the bound only stops a broken test hanging.
    async fn wait_held_count(gate: &UrlGate, count: usize) -> bool {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if super::lock(&gate.held).len() >= count {
                    return true;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or(false)
    }

    /// Run `read` across a held match: poll it once so its cutoff is fixed, let the action's
    /// grace pass, poll it again so its wait is running, then give `gate` one permit and let the
    /// read finish.
    async fn read_while_held<T>(gate: &UrlGate, read: impl Future<Output = T>) -> T {
        let mut read = std::pin::pin!(read);
        if let std::task::Poll::Ready(value) = futures::poll!(read.as_mut()) {
            return value;
        }
        tokio::time::sleep(ACTION_GRACE * 2).await;
        if let std::task::Poll::Ready(value) = futures::poll!(read.as_mut()) {
            return value;
        }
        gate.permits.add_permits(1);
        read.await
    }

    /// Send a request to a denied address from a watched page, and `read` its refusal. When
    /// `held`, the request's match is held until the read is waiting and the action's grace has
    /// passed; otherwise the request is matched at once and read once it is refused. Returns the
    /// denied URL, whether the listener had received the request before the read, the refused
    /// URL `read` saw, and whether every pause was released once the check stopped.
    async fn refused_after_a_match(
        browser: &Arc<Browser>,
        held: bool,
        read: Read,
    ) -> (String, bool, Option<String>, bool) {
        let gate = closed_match_gate();
        let (url, _hits) = denied_listener().await;
        let delays = TestDelays {
            match_gate: held.then(|| (Arc::clone(&gate), url.clone())),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(browser, delays).await;
        let shared = Arc::clone(&watch.shared);
        let started = Instant::now();
        let _ = page
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let received = if held {
            wait_held(&gate, &url).await
        } else {
            wait_for_refusal(&watch, &url).await
        };
        let refused = match read {
            Read::Action => read_while_held(&gate, watch.refusal_during(started, ACTION_GRACE))
                .await
                .map(|(refused_url, _)| refused_url),
            Read::RefusedUrls => read_while_held(&gate, watch.refused_urls()).await.into_iter().next(),
        };
        gate.permits.close();
        watch.close().await;
        firewall.stop().await;
        let released = lock(&shared.unmatched).is_empty();
        (url, received, refused, released)
    }

    /// A request refused after its frame was matched slowly still counts for the action that
    /// sent it. A request is counted from the pause, not from the match: matching a frame the
    /// registry does not know yet looks through the frame trees of every live target, which can
    /// take far longer than the grace after an action.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_whose_frame_was_matched_slowly_counts_for_the_action_that_sent_it() {
        let test_name = "a_refusal_whose_frame_was_matched_slowly_counts_for_the_action_that_sent_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (url, received, refused, _) = refused_after_a_match(&browser, true, Read::Action).await;
        close(browser).await;
        assert!(
            received,
            "{test_name}: the check must hold the request in its match, or the test shows nothing"
        );
        assert_eq!(
            refused,
            Some(url),
            "{test_name}: the refusal must count for the action that sent the request"
        );
    }

    /// The control of the test above: with no hold in the match, the refusal counts for the
    /// action, so the hold is what the test above measures. Every pause is released once the
    /// check has answered it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_whose_frame_was_matched_at_once_counts_for_the_action_that_sent_it() {
        let test_name = "a_refusal_whose_frame_was_matched_at_once_counts_for_the_action_that_sent_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (url, received, refused, released) = refused_after_a_match(&browser, false, Read::Action).await;
        close(browser).await;
        assert!(
            received,
            "{test_name}: the check must refuse the request before it is read, or the test shows nothing"
        );
        assert_eq!(refused, Some(url), "{test_name}: the refusal must count for the action");
        assert!(
            released,
            "{test_name}: every pause must be released once it is answered"
        );
    }

    /// A request refused after its frame was matched slowly is on the page's refused URLs when
    /// they are read while the match still runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_whose_frame_was_matched_slowly_is_listed_on_the_result() {
        let test_name = "a_refusal_whose_frame_was_matched_slowly_is_listed_on_the_result";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (url, received, refused, _) = refused_after_a_match(&browser, true, Read::RefusedUrls).await;
        close(browser).await;
        assert!(
            received,
            "{test_name}: the check must hold the request in its match, or the test shows nothing"
        );
        assert_eq!(refused, Some(url), "{test_name}: the refused URL must be listed");
    }

    /// A watched page whose requests to the denied URL are held in their match, each until the
    /// test gives the gate a permit, with the gate and the denied URL.
    async fn held_matching_page(
        browser: &Arc<Browser>,
    ) -> (BrowserFirewall, chromiumoxide::Page, super::Watch, Arc<UrlGate>, String) {
        let gate = closed_match_gate();
        let (url, _hits) = denied_listener().await;
        let delays = TestDelays {
            match_gate: Some((Arc::clone(&gate), url.clone())),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(browser, delays).await;
        (firewall, page, watch, gate, url)
    }

    /// The wait after an action does not hold on a request paused before the action began: it
    /// cannot be the action's refusal. The request is still being matched when the wait ends.
    ///
    /// ~keep The earlier request is held in its match for the whole test, so a wait that held on
    /// ~keep it would run to `ACTION_SETTLE_LIMIT`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_wait_after_an_action_skips_a_request_paused_before_it() {
        let test_name = "the_wait_after_an_action_skips_a_request_paused_before_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch, gate, url) = held_matching_page(&browser).await;
        let _ = page
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let earlier_held = wait_held(&gate, &url).await;
        let started = Instant::now();
        let ended = tokio::time::timeout(ACTION_SETTLE_LIMIT / 2, watch.refusal_during(started, ACTION_GRACE)).await;
        gate.permits.close();
        watch.close().await;
        firewall.stop().await;
        close(browser).await;
        assert!(
            earlier_held,
            "{test_name}: the check must hold the earlier request in its match, or the test shows nothing"
        );
        assert!(
            ended.is_ok(),
            "{test_name}: the wait must end while the earlier request is still being matched"
        );
        assert_eq!(
            ended.ok().flatten(),
            None,
            "{test_name}: the earlier request is not the action's"
        );
    }

    /// The wait after an action does not hold on a request paused after its cutoff: that one
    /// counts for the next action. The wait ends once the action's own request is judged, while
    /// the later one is still being matched.
    ///
    /// ~keep The later request is sent only after the cutoff is fixed, and is held in its match
    /// ~keep for the whole test, so a wait that held on it would run to `ACTION_SETTLE_LIMIT`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_wait_after_an_action_skips_a_request_paused_after_its_cutoff() {
        let test_name = "the_wait_after_an_action_skips_a_request_paused_after_its_cutoff";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch, gate, url) = held_matching_page(&browser).await;
        let later = format!("{url}?later");
        let started = Instant::now();
        let _ = page
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let own_held = wait_held(&gate, &url).await;
        let mut counting = Box::pin(watch.refusal_during(started, ACTION_GRACE));
        let _ = futures::poll!(&mut counting);
        tokio::time::sleep(ACTION_GRACE * 2).await;
        let _ = page
            .evaluate(format!("fetch({later:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let later_held = wait_held(&gate, &later).await;
        gate.permits.add_permits(1);
        let ended = tokio::time::timeout(ACTION_SETTLE_LIMIT / 2, &mut counting).await;
        let in_time = ended.is_ok();
        let refused = match ended {
            Ok(refused) => refused,
            Err(_) => (&mut counting).await,
        };
        drop(counting);
        gate.permits.close();
        watch.close().await;
        firewall.stop().await;
        close(browser).await;
        assert!(
            own_held && later_held,
            "{test_name}: the check must hold both requests in their match, or the test shows nothing"
        );
        assert_eq!(
            refused.map(|(refused_url, _)| refused_url),
            Some(url),
            "{test_name}: the action's own request must be its refusal"
        );
        assert!(
            in_time,
            "{test_name}: the wait must end while the later request is still being matched"
        );
    }

    /// A request of a tab the check does not watch releases its count once it is answered, so a
    /// later wait of a watched page does not hold on it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_of_a_tab_outside_the_check_releases_its_count_at_once() {
        let test_name = "a_request_of_a_tab_outside_the_check_releases_its_count_at_once";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch) = watched_page(&browser, TestDelays::default()).await;
        let other = browser.new_page("about:blank").await.expect("page");
        let (url, _hits) = denied_listener().await;
        let answered: String = other
            .evaluate(format!(
                "fetch({url:?}, {{ mode: 'no-cors' }}).then(() => 'reached', () => 'refused')"
            ))
            .await
            .ok()
            .and_then(|result| result.into_value().ok())
            .unwrap_or_default();
        let released = lock(&watch.shared.unmatched).is_empty();
        watch.close().await;
        firewall.stop().await;
        drop((page, other));
        close(browser).await;
        assert_eq!(
            answered, "refused",
            "{test_name}: the check must refuse the tab's request before the count is read, or the test shows nothing"
        );
        assert!(
            released,
            "{test_name}: the tab's request must release its count once it is answered"
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
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
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
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        // The two script navigations below count against the redirect limit.
        let watch = firewall
            .handle()
            .watch(&page, &config(), 2)
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

    /// A new page request after stop begins is refused before creating a context or target.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_watched_while_the_check_stops_is_refused() {
        let test_name = "a_page_watched_while_the_check_stops_is_refused";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        for context in [PageContext::Isolated, PageContext::Shared] {
            let gate = Arc::new(tokio::sync::Semaphore::new(1));
            let delays = TestDelays {
                deliver_gate: Some(Arc::clone(&gate)),
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
            let late = handle.new_page(None, None).await;
            let stopping_pending = !stopping.is_finished();
            gate.add_permits(1);
            let _ = stopping.await;
            let after = context_count(&browser).await;
            drop((watch, page));
            assert!(
                stopping_pending,
                "{test_name} ({context:?}): stop must still be pending when the open is refused"
            );
            let refusal = late.err().map(|error| error.to_string()).unwrap_or_default();
            assert!(
                refusal.contains("request interception stopped"),
                "{test_name} ({context:?}): a page requested during stop must fail as stopped, got {refusal:?}"
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
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let closed = page.clone();
        let _ = page.close().await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(closed.mainframe().await, Ok(Some(_))) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let frame_gone = !matches!(closed.mainframe().await, Ok(Some(_)));
        let watched = firewall.handle().watch(&closed, &config(), 0).await;
        firewall.stop().await;
        drop(closed);
        close(browser).await;
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

    /// Close `browser` once the test is done with it, and kill it if it has not exited within five
    /// seconds.
    ///
    /// ~keep `Browser::close` and `Browser::wait` have no bound of their own, so a Chrome that does
    /// ~keep not exit would hold the test, and the macOS job with it, until the job timeout
    /// ~keep (xberg-io/crawlberg#573).
    async fn close(browser: Arc<Browser>) {
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = crate::browser_pool::close_browser_within(&mut browser, Duration::from_secs(5)).await;
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
        // ~keep `maybe_done` keeps a park or a stop that ended in its first poll. Awaiting the bare
        // ~keep future again panics, which hid the assertion below that names this case.
        let mut parking = Box::pin(futures::future::maybe_done(watch.park()));
        let park_held = futures::poll!(&mut parking).is_pending();
        let mut stopping = Box::pin(futures::future::maybe_done(firewall.stop()));
        let stop_held = futures::poll!(&mut stopping).is_pending();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10), async {
            parking.await;
            stopping.await;
        })
        .await
        .expect("stopping the check must finish");
        let reached = served(&denied_hits).await;
        let still_open = target_is_open(&browser, &target)
            .await
            .expect("the browser must answer after the check stops");
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
        let keepalive = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
        let page = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
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
            page_open = target_is_open(&browser, &target)
                .await
                .expect("the browser must answer while the page closes");
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
        let opened = handle.new_page(None, None).await;
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
        let closed = !target_is_open(&browser, page.target_id())
            .await
            .expect("the browser must answer after the watch closes");
        let in_browser = cookie_names(&browser, None).await;
        let next = firewall
            .handle()
            .new_page(None, None)
            .await
            .expect("the check must open a page");
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
        // ~keep `maybe_done` keeps a park or a stop that ended in its first poll. Awaiting the bare
        // ~keep future again panics, which hid the assertion below that names this case.
        let mut parking = Box::pin(futures::future::maybe_done(watch.park()));
        let park_held = futures::poll!(&mut parking).is_pending();
        let mut stopping = Box::pin(futures::future::maybe_done(firewall.stop()));
        let stop_held = futures::poll!(&mut stopping).is_pending();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10), async {
            parking.await;
            stopping.await;
        })
        .await
        .expect("stopping the check must finish");
        let reached = served(&denied_hits).await;
        let still_open = target_is_open(&browser, &target)
            .await
            .expect("the browser must answer after the check stops");
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
            let page = firewall
                .handle()
                .new_page(None, None)
                .await
                .expect("the check must open a page");
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
        let open_while_watched = target_is_open(&browser, &target)
            .await
            .expect("the browser must answer while the page is watched");
        drop(watch);
        let mut still_open = true;
        for _ in 0..50 {
            still_open = target_is_open(&browser, &target)
                .await
                .expect("the browser must answer while the watch closes");
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
        // ~keep `maybe_done`: the stop is awaited again below, also when this poll already ended it.
        let mut stopping = Box::pin(futures::future::maybe_done(firewall.stop()));
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

    /// Pages under one SSRF policy share one SSRF proxy; a page under another allowlist gets its own.
    #[tokio::test(flavor = "multi_thread")]
    async fn pages_under_one_policy_share_one_ssrf_proxy() {
        let test_name = "pages_under_one_policy_share_one_ssrf_proxy";
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
        let policy = config().ssrf;
        let other = crate::types::CrawlConfig::builder()
            .allow_private_networks(false)
            .build()
            .ssrf;
        let handle = firewall.handle();
        let first = handle
            .new_page(None, Some(&policy))
            .await
            .expect("the check must open a page");
        let second = handle
            .new_page(None, Some(&policy))
            .await
            .expect("the check must open a page");
        let shared = handle.egress.lock().await.len();
        let third = handle
            .new_page(None, Some(&other))
            .await
            .expect("the check must open a page");
        let separate = handle.egress.lock().await.len();
        drop((first, second, third));
        firewall.stop().await;
        close(browser).await;
        assert!(policy.deny_private, "{test_name}: the test needs deny_private on");
        assert_eq!(
            shared, 1,
            "{test_name}: two pages under one policy must share one SSRF proxy, got {shared}"
        );
        assert_eq!(
            separate, 2,
            "{test_name}: a page under another allowlist must get its own SSRF proxy, got {separate}"
        );
    }
}
