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
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, EventTargetDestroyed, GetTargetsParams, TargetId,
};
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, oneshot};

use crate::error::CrawlError;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::ssrf::{SsrfPolicy, validate_url};

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
/// that answers every paused request of every target in that browser, for every page that
/// works in it. Pages register with [`FirewallHandle::watch`]; interception is enabled while
/// at least one page is watched.
///
/// ~keep CDP Fetch interception is per session. Enabled on a page's session it pauses only
/// ~keep that page's requests, and chromiumoxide attaches a popup's target without pausing it,
/// ~keep so the popup's first request would leave before a page-level interception could be
/// ~keep enabled on it. Enabled on the browser session, it pauses every target's requests.
/// ~keep A browser serves several pages at once (a `BrowserPool` hands out one tab per
/// ~keep concurrent fetch), and a second Fetch listener on the same session would answer
/// ~keep the same paused requests and turn interception off under the others, so one
/// ~keep listener per browser serves them all. On a browser reached through
/// ~keep `browser.endpoint`, the check also covers pages other clients opened, while a page
/// ~keep of ours is watched.
pub(crate) struct BrowserFirewall {
    handle: FirewallHandle,
    listener: tokio::task::JoinHandle<()>,
}

/// A cheap, cloneable reference to a [`BrowserFirewall`], used to watch pages.
#[derive(Clone)]
pub(crate) struct FirewallHandle {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
}

/// A page under the check. Its requests are judged by its own policy, its main-frame
/// redirects are counted against its limit, and the requests the check blocked are recorded.
/// [`Watch::close`] or [`Watch::park`] ends it; dropping it closes the page like `close`.
pub(crate) struct Watch {
    commands: mpsc::UnboundedSender<Command>,
    page: Arc<WatchedPage>,
    ended: bool,
}

struct Shared {
    pages: Mutex<Vec<Arc<WatchedPage>>>,
}

struct WatchedPage {
    target: TargetId,
    main_frame: FrameId,
    policy: SsrfPolicy,
    redirect_limit: usize,
    outcome: Mutex<InterceptOutcome>,
}

enum Command {
    Enable(oneshot::Sender<Result<(), String>>),
    Disable,
    /// Close the page's popups, and the page itself when `close_page` is set, then stop
    /// watching it.
    End {
        page: Arc<WatchedPage>,
        close_page: bool,
        done: Option<oneshot::Sender<()>>,
    },
}

impl BrowserFirewall {
    /// Start the listener on `browser`'s session. Interception stays off until a page is watched.
    pub(crate) async fn start(browser: Arc<Browser>) -> Result<Self, CrawlError> {
        let events = browser
            .event_listener::<EventRequestPaused>()
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;
        let shared = Arc::new(Shared {
            pages: Mutex::new(Vec::new()),
        });
        let (commands, receiver) = mpsc::unbounded_channel();
        let listener = tokio::spawn(serve(browser, events, receiver, commands.clone(), Arc::clone(&shared)));
        Ok(Self {
            handle: FirewallHandle { shared, commands },
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
            target: page.target_id().clone(),
            main_frame,
            policy: policy.clone(),
            redirect_limit,
            outcome: Mutex::new(InterceptOutcome::default()),
        });
        let enabled = {
            let mut pages = lock(&self.shared.pages);
            pages.push(Arc::clone(&watched));
            // ~keep The command is queued under the lock, so the listener sees enables and
            // ~keep disables in the order the watched set became non-empty and empty.
            (pages.len() == 1).then(|| {
                let (ack, done) = oneshot::channel();
                let _ = self.commands.send(Command::Enable(ack));
                done
            })
        };
        let watch = Watch {
            commands: self.commands.clone(),
            page: watched,
            ended: false,
        };
        if let Some(done) = enabled {
            match done.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    return Err(CrawlError::browser_error(format!(
                        "failed to enable request interception: {e}"
                    )));
                }
                Err(_) => return Err(CrawlError::browser_error("request interception stopped")),
            }
        }
        Ok(watch)
    }
}

impl Watch {
    /// Return what the check observed for this page so far, and keep watching. Redirects are
    /// not counted again: once the main frame has its first document, later navigations are
    /// free of the redirect limit.
    pub(crate) fn take_outcome(&self) -> InterceptOutcome {
        let mut state = lock(&self.page.outcome);
        let first_document_arrived = state.first_document_arrived;
        let outcome = std::mem::take(&mut *state);
        state.first_document_arrived = first_document_arrived;
        outcome
    }

    /// Close the page and every popup it opened, and end the watch once Chrome has
    /// destroyed them. The check answers their requests until then.
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
    // ~keep A watch dropped mid-fetch (a timeout, a cancelled future) still closes its pages
    // ~keep under the check: the listener runs the close, and only then stops watching.
    fn drop(&mut self) {
        if !self.ended {
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

/// The listener: switch interception on and off in the order asked, and answer the paused
/// requests and close popups concurrently, so a slow DNS lookup or a closing popup of one
/// page does not hold up the others.
async fn serve(
    browser: Arc<Browser>,
    mut events: chromiumoxide::listeners::EventStream<EventRequestPaused>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    sender: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
) {
    let browser = &*browser;
    let shared = &*shared;
    let sender = &sender;
    let mut running: FuturesUnordered<BoxFuture<'_, ()>> = FuturesUnordered::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Enable(ack)) => {
                    let result = browser.execute(fetch_enable_params()).await;
                    let _ = ack.send(result.map(drop).map_err(|e| e.to_string()));
                }
                Some(Command::Disable) => {
                    let _ = browser.execute(FetchDisableParams::default()).await;
                }
                Some(Command::End { page, close_page, done }) => running.push(Box::pin(async move {
                    close_targets(browser, &page.target, close_page).await;
                    let mut pages = lock(&shared.pages);
                    pages.retain(|watched| !Arc::ptr_eq(watched, &page));
                    // ~keep Queued under the lock, like the enable in `watch`, so the two stay in order.
                    if pages.is_empty() {
                        let _ = sender.send(Command::Disable);
                    }
                    drop(pages);
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                })),
                None => break,
            },
            event = events.next() => match event {
                Some(event) => running.push(Box::pin(answer(browser, shared, event))),
                None => break,
            },
            Some(()) = running.next(), if !running.is_empty() => {}
        }
    }
}

/// How long closing a watched page and its popups may take before the watch ends anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Close every page `root` opened, directly or through another popup, and `root` itself when
/// `close_root` is set, then wait until Chrome has destroyed them. A page already closed is
/// skipped.
async fn close_targets(browser: &Browser, root: &TargetId, close_root: bool) {
    let Ok(mut destroyed) = browser.event_listener::<EventTargetDestroyed>().await else {
        return;
    };
    let targets = match browser.execute(GetTargetsParams::default()).await {
        Ok(response) => response.result.target_infos,
        Err(_) => Vec::new(),
    };
    let mut open: Vec<TargetId> = targets
        .iter()
        .filter(|target| target.target_id == *root)
        .map(|target| target.target_id.clone())
        .collect();
    let mut index = 0;
    while index < open.len() {
        let opener = open[index].clone();
        open.extend(
            targets
                .iter()
                .filter(|target| target.opener_id.as_ref() == Some(&opener))
                .map(|target| target.target_id.clone()),
        );
        index += 1;
    }
    if !close_root {
        open.retain(|target| target != root);
    }
    // ~keep Popups close before the page that opened them: closing the opener first let a
    // ~keep popup's request through the check in some runs.
    for target in open.iter().rev() {
        let _ = browser.execute(CloseTargetParams::new(target.clone())).await;
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        while !open.is_empty() {
            let Some(event) = destroyed.next().await else {
                return;
            };
            open.retain(|target| *target != event.target_id);
        }
    })
    .await;
}

/// Answer one paused request. A document response of a watched main frame is judged by
/// [`main_frame_verdict`]. A request is judged by the policy of the page whose main frame sent
/// it; a request of any other frame, worker or popup must pass the policy of every watched
/// page, and a refusal is recorded on each page whose policy refused it. With no page
/// watched, every request is refused.
async fn answer(browser: &Browser, shared: &Shared, event: Arc<EventRequestPaused>) {
    let pages = lock(&shared.pages).clone();
    let owner = pages.iter().find(|page| page.main_frame == event.frame_id);

    let allow = if is_response_stage(&event) {
        owner.is_none_or(|page| main_frame_verdict(&event, page.redirect_limit, &page.outcome))
    } else {
        let judges = owner.map_or(pages.as_slice(), std::slice::from_ref);
        let mut allow = !judges.is_empty();
        for page in judges {
            if let Err(reason) = ssrf_verdict(&event.request.url, &page.policy).await {
                allow = false;
                let mut outcome = lock(&page.outcome);
                if outcome.blocked.is_none() {
                    outcome.blocked = Some((event.request.url.clone(), reason));
                }
            }
        }
        allow
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

/// Whether a paused document response of a watched main frame may proceed. A redirect of the
/// requested navigation is counted while it is within `limit`; the one past it is recorded and
/// must be failed. A response Chrome does not commit is also recorded and failed.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started, and it is not counted.
/// ~keep Chrome commits no document for a 204, 205 or 304, so no load event fires and
/// ~keep chromiumoxide's `goto` waits for the browser timeout. Failing the response makes
/// ~keep Chrome commit its error page, which ends `goto` at once.
fn main_frame_verdict(event: &EventRequestPaused, limit: usize, state: &Mutex<InterceptOutcome>) -> bool {
    let mut state = lock(state);
    if state.first_document_arrived {
        return true;
    }

    let headers = event.response_headers.as_deref().unwrap_or_default();
    let status = event.response_status_code.and_then(|code| u16::try_from(code).ok());
    let is_redirect = status.is_some_and(|code| REDIRECT_STATUSES.contains(&code))
        && headers.iter().any(|h| h.name.eq_ignore_ascii_case("location"));
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
