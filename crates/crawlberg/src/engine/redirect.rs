//! Redirect chain following and the policy every hop must satisfy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use regex::Regex;
use url::Url;

use super::CrawlEngine;
use super::robots_cache::RobotsCacheKey;
use crate::error::CrawlError;
use crate::helpers::RobotsOutcome;
use crate::helpers::fetch_robots_outcome;
use crate::html::{detect_meta_refresh, effective_base_url, mask_raw_text_markup, refresh_target};
use crate::html::{is_fetchable_scheme, is_html_content};
use crate::net::redact_url_credentials;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::normalize::{normalize_url_for_dedup, resolve_redirect};

/// Outcome of a [`follow_redirects`] call.
pub(crate) struct RedirectOutcome {
    /// The final URL after all redirects have been followed.
    pub(crate) final_url: String,
    /// The HTTP response at the final URL.
    pub(crate) final_response: crate::tower::CrawlResponse,
    /// Number of redirect hops taken.
    pub(crate) redirect_count: usize,
    /// `(host, response headers)` for each intermediate redirect hop, so cookie extraction
    /// can validate each hop's `Set-Cookie` `Domain=` attribute against the host that sent it.
    pub(crate) intermediate_headers: Vec<(String, HashMap<String, Vec<String>>)>,
    /// Whether headless-browser fetch was used for the final hop.
    pub(crate) browser_used: bool,
}

/// Best-effort host extraction for `Set-Cookie` `Domain=` validation.
///
/// An unparsable `url` yields an empty host, which `validate_cookie_domain` never
/// domain-matches against a non-empty `Domain=` attribute, so a malformed hop URL causes
/// any explicitly-scoped cookie from it to be rejected rather than silently accepted.
pub(super) fn url_host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What a [`follow_redirects`] call produced.
pub(crate) enum RedirectResolution {
    /// The chain ended on a response.
    Fetched(RedirectOutcome),
    /// The policy refused a URL in the chain, so it was never requested.
    Refused {
        /// Why the URL was refused.
        refusal: PolicyRefusal,
        /// ~keep Hops already followed before the refusal. Carried out so a crawl refused at
        /// ~keep hop 3 does not report `redirect_count: 0` and understate what it did.
        redirect_count: usize,
        /// ~keep Headers from the hops already made, so a refusal does not discard cookies
        /// ~keep the chain legitimately collected before it was stopped.
        intermediate_headers: Vec<(String, HashMap<String, Vec<String>>)>,
    },
}

/// Why [`RedirectPolicy`] refused a URL.
pub(crate) enum PolicyRefusal {
    /// robots.txt forbids it, with the reason the crawl reports.
    Blocked {
        /// The refused URL, which the result reports as the crawl's final URL.
        url: String,
        /// The reason, from [`robots_block_reason`].
        reason: String,
    },
    /// A path filter rejects it. The crawl reports no error, matching the loop's filter.
    Filtered {
        /// The refused URL, which the result reports as the crawl's final URL.
        url: String,
    },
}

impl PolicyRefusal {
    /// The URL to report as the crawl's final URL, and the error to report with it.
    pub(super) fn into_parts(self) -> (String, Option<String>) {
        match self {
            Self::Blocked { url, reason } => (url, Some(reason)),
            Self::Filtered { url } => (url, None),
        }
    }

    /// The refusal as an error, for a caller with no place to report a URL it skipped.
    pub(crate) fn into_error(self) -> CrawlError {
        match self {
            Self::Blocked { reason, .. } => CrawlError::other(reason),
            Self::Filtered { url } => CrawlError::other(format!("{url} is excluded from this crawl")),
        }
    }
}

/// The crawl's per-URL policy: the path filters, then robots.txt for the URL's own origin.
///
/// ~keep [`follow_redirects`] consults this immediately before every request it makes, so a
/// ~keep redirect target is judged by the rules of the origin it belongs to and is refused
/// ~keep before the request goes out. Judging the chain at its call site instead reads the
/// ~keep seed's file, fetches the whole chain, and only then asks whether the target allowed
/// ~keep it, which is one request too late.
pub(crate) struct RedirectPolicy<'a> {
    pub(super) engine: &'a CrawlEngine,
    pub(super) client: &'a reqwest::Client,
    pub(super) exclude_regexes: &'a [Regex],
    /// ~keep Applied only to genuine redirect targets (`is_redirect_hop`), never to the
    /// ~keep chain's own starting URL: that URL already passed whatever include check applied
    /// ~keep to it (none, if it is the seed at depth 0) before this policy ever saw it, exactly
    /// ~keep as `claim_redirect_target` only dedups a redirect target and not the chain's seed.
    pub(super) include_regexes: &'a [Regex],
    /// Whether `include_paths`/`exclude_paths` match `path?query` instead of just `path`.
    pub(super) match_query: bool,
    /// What robots.txt established per origin, so one origin's file is read once per crawl.
    pub(super) outcomes: HashMap<RobotsCacheKey, Arc<RobotsOutcome>>,
    /// The origin of the last URL admitted, whose rules the crawl loop keeps applying.
    pub(super) last_origin: Option<RobotsCacheKey>,
    /// URLs this policy rejected, folded into `CrawlState::urls_filtered`.
    pub(super) urls_filtered: usize,
    /// The agent [`Self::admits`] chose for the most recently admitted URL.
    ///
    /// ~keep Set once per hop, by the same call that used it for robots.txt group selection.
    /// `follow_redirects` reads this after a successful `admits()` and pins it onto that hop's
    /// `CrawlRequest`, so the request the robots decision was made for and the request that
    /// actually goes out (including its retries and escalations, which re-send the same
    /// request rather than starting a new one) are the same agent (crawlberg#423).
    pub(super) pending_user_agent: Option<String>,
}

impl<'a> RedirectPolicy<'a> {
    pub(super) fn new(
        engine: &'a CrawlEngine,
        client: &'a reqwest::Client,
        exclude_regexes: &'a [Regex],
        include_regexes: &'a [Regex],
    ) -> Self {
        Self {
            engine,
            client,
            exclude_regexes,
            include_regexes,
            match_query: engine.config.path_patterns_match_query,
            outcomes: HashMap::new(),
            last_origin: None,
            urls_filtered: 0,
            pending_user_agent: None,
        }
    }

    /// Decide whether the crawl may request `url`.
    ///
    /// `is_redirect_hop` is `true` once at least one redirect has already been followed in
    /// this chain -- i.e. `url` is a hop the chain landed on, not the chain's own starting
    /// URL. Only then is `url` checked against the frontier's seen-set: the starting URL was
    /// already claimed by whichever discovery step enqueued it, so checking it here would
    /// refuse every chain as a "duplicate" of itself.
    ///
    /// `Ok(None)` admits it, `Ok(Some(refusal))` refuses it, and `Err` is an engine failure
    /// (a rate-limiter backend error), which is not a policy decision and must not be
    /// reported as one.
    pub(super) async fn admits(
        &mut self,
        url: &str,
        is_redirect_hop: bool,
    ) -> Result<Option<PolicyRefusal>, CrawlError> {
        // ~keep A URL this cannot parse is refused, not admitted. Letting it through would
        // ~keep skip robots entirely on the strength of a parse failure, and this is the
        // ~keep component that decides whether a request may go out at all -- the same
        // ~keep fail-closed rule that governs `outcome_for_fetch_error`, where the catch-all
        // ~keep arm has to be the closed one for the guarantee to hold.
        // ~keep Both refusals below name the address through the redactor: a hostless value
        // ~keep such as `user:token@host` is exactly the one that carries a credential.
        let Ok(parsed) = Url::parse(url) else {
            return Ok(Some(PolicyRefusal::Blocked {
                url: url.to_owned(),
                reason: format!(
                    "robots_unreachable: cannot parse {} to determine its origin",
                    redact_url_credentials(url)
                ),
            }));
        };
        // ~keep `robots_origin_key` falls back to an empty host, so every hostless URL would
        // ~keep share one cache entry and inherit an unrelated origin's rules.
        if parsed.host_str().is_none() {
            return Ok(Some(PolicyRefusal::Blocked {
                url: url.to_owned(),
                reason: format!(
                    "robots_unreachable: {} has no host to read robots.txt from",
                    redact_url_credentials(url)
                ),
            }));
        }

        // ~keep The path filters first: they are local, and an excluded URL should not cost
        // ~keep its origin a robots.txt request either. See the field doc on
        // ~keep `include_regexes` for why `is_redirect_hop` gates the include check.
        if let Some(refusal) = self.filtered_by_path_patterns(url, &parsed, is_redirect_hop) {
            return Ok(Some(refusal));
        }

        // ~keep Chosen once per hop, here, before robots.txt is even read: the same call that
        // ~keep advances the UA rotation counter, so this hop's robots decision and the agent
        // ~keep `follow_redirects` pins onto its `CrawlRequest` afterward are the same pick
        // ~keep (crawlberg#423). Without a configured rotation list this is exactly
        // ~keep `default_robots_user_agent`, so a non-rotating crawl sees no change.
        let user_agent = self.engine.choose_request_user_agent();
        let origin = RobotsCacheKey::new(&parsed, &user_agent);
        let first_visit = !self.outcomes.contains_key(&origin);
        if first_visit {
            let outcome = resolve_robots_outcome(self.engine, self.client, &parsed, url, &user_agent).await;
            self.outcomes.insert(origin.clone(), outcome);
        }
        let outcome = self
            .outcomes
            .get(&origin)
            .expect("the origin's robots.txt outcome was just read");
        if let Some(reason) = robots_block_reason(outcome, &parsed) {
            return Ok(Some(PolicyRefusal::Blocked {
                url: url.to_owned(),
                reason,
            }));
        }

        if is_redirect_hop && let Some(refusal) = self.claim_redirect_target(url).await? {
            return Ok(Some(refusal));
        }

        // ~keep Published once per origin, after the origin is admitted and before the
        // ~keep request this call precedes. The call this replaces ran once for the seed
        // ~keep before the chain and once for a changed final origin; doing it here covers
        // ~keep every origin in the chain instead, and keeps the delay ahead of the request
        // ~keep rather than behind it. Publishing it before the block check above would make
        // ~keep a refused URL wait out a delay for a request that is never sent.
        if first_visit {
            self.engine.apply_crawl_delay(outcome, &parsed).await?;
        }
        self.last_origin = Some(origin);
        self.pending_user_agent = Some(user_agent);
        Ok(None)
    }

    /// Whether `url` is filtered by `exclude_paths`/`include_paths`, as [`PolicyRefusal::Filtered`].
    fn filtered_by_path_patterns(&mut self, url: &str, parsed: &Url, is_redirect_hop: bool) -> Option<PolicyRefusal> {
        let admitted = crate::helpers::passes_path_patterns(
            parsed,
            self.exclude_regexes,
            self.include_regexes,
            is_redirect_hop,
            self.match_query,
            &mut self.urls_filtered,
        );
        (!admitted).then(|| PolicyRefusal::Filtered { url: url.to_owned() })
    }

    /// Claim `url` -- a redirect hop, never the chain's own starting URL -- against the
    /// frontier's seen-set.
    ///
    /// ~keep Deduplicates against the frontier's own seen-set rather than a set local to this
    /// ~keep policy: a page reachable both directly (its own frontier entry) and via a
    /// ~keep redirect must be requested once, and the frontier is the one place both paths
    /// ~keep already agree on what "seen" means.
    async fn claim_redirect_target(&self, url: &str) -> Result<Option<PolicyRefusal>, CrawlError> {
        let dedup_key = normalize_url_for_dedup(url, self.engine.config.dedup_include_query);
        if self.engine.frontier.is_seen(&dedup_key).await? {
            return Ok(Some(PolicyRefusal::Filtered { url: url.to_owned() }));
        }
        self.engine.frontier.mark_seen(&dedup_key).await?;
        Ok(None)
    }

    /// What robots.txt established for the origin the chain ended on, which the crawl loop
    /// applies to every page it fetches from there.
    pub(super) fn into_outcome(mut self) -> Arc<RobotsOutcome> {
        self.last_origin
            .and_then(|origin| self.outcomes.remove(&origin))
            .unwrap_or_else(|| Arc::new(RobotsOutcome::AllowAll))
    }
}

/// Resolve the robots.txt outcome `agent` sees at `parsed`'s origin: the shared cache when
/// `respect_robots_txt` is on and the request carries no credentials, a direct fetch for a
/// credentialed request (never shared with another caller of the same origin), and
/// `AllowAll` when robots.txt is off.
///
/// ~keep `pub(super)`: the one place that resolves a robots.txt outcome for an agent, shared by
/// `admits` (judging the agent chosen for the current tier) and
/// `engine/dispatch.rs::run_tier`'s `Tier::Browser` arm (re-judging the browser's own agent on
/// escalation), so the two can never resolve the same `(origin, agent)` two different ways
/// (crawlberg#423).
pub(super) async fn resolve_robots_outcome(
    engine: &CrawlEngine,
    client: &reqwest::Client,
    parsed: &Url,
    url: &str,
    agent: &str,
) -> Arc<RobotsOutcome> {
    if !engine.config.respect_robots_txt {
        return Arc::new(RobotsOutcome::AllowAll);
    }
    // ~keep A robots.txt read with the caller's credentials is theirs alone: the shared cache
    // ~keep would hand it to the next crawl of the same origin.
    if crate::net::credentials::is_credentialed(&engine.config, parsed) {
        return Arc::new(fetch_robots_outcome(url, &engine.config, client, agent).await);
    }
    let key = RobotsCacheKey::new(parsed, agent);
    engine
        .robots_cache
        .get_or_fetch(key, || fetch_robots_outcome(url, &engine.config, client, agent))
        .await
}

/// The reason robots.txt forbids fetching `parsed` at all, if it does.
///
/// ~keep `pub(super)`: also read by `engine/dispatch.rs::run_tier`'s `Tier::Browser` arm, which
/// judges the same outcome shape against the browser's own agent right before it fetches
/// (crawlberg#423).
pub(super) fn robots_block_reason(robots: &RobotsOutcome, parsed: &Url) -> Option<String> {
    if let Some(reason) = robots.disallow_all_reason() {
        return Some(format!("robots_unreachable: {reason}"));
    }
    if !robots.allows(parsed.path()) {
        return Some(format!("robots.txt disallows {}", parsed.path()));
    }
    None
}

/// Canonicalize a URL for redirect-cycle-set membership.
///
/// ~keep The seed comes from the caller's raw string, but every hop key comes from
/// ~keep `resolve_redirect`, which WHATWG-serializes via `Url::join` (e.g. adding a
/// ~keep trailing slash to a bare origin). Without canonicalizing the seed the same
/// ~keep way, a chain that returns to the seed URL in a different-but-equivalent form
/// ~keep (e.g. `http://host:port` vs `http://host:port/`) is missed by `seen.contains`
/// ~keep on its first return and only caught one hop later. Falls back to the original
/// ~keep string when it fails to parse, so an unparsable URL still participates in
/// ~keep cycle detection via literal string equality.
fn canonical_redirect_key(url: &str) -> String {
    Url::parse(url)
        .map(|parsed| parsed.to_string())
        .unwrap_or_else(|_| url.to_owned())
}

/// Follow HTTP 3xx, `Refresh` header, and `<meta http-equiv="refresh">` redirects.
///
/// This is the shared redirect-following implementation used by both
/// [`CrawlEngine::scrape`] and the initial-redirect phase of
/// [`CrawlEngine::crawl`]. The global reqwest redirect policy remains
/// `Policy::none()` — this function performs manual redirect resolution so
/// that the crawl loop retains full control over the redirect chain.
///
/// # Errors
///
/// Returns `Err` only on network-level failures (DNS, connection refused, timeout, …).
/// Reaching `max_redirects` or detecting a cycle is **not** an error — the loop
/// stops and the most recent response (the 3xx that would have redirected further)
/// is returned to the caller. This matches the historical behavior of
/// [`CrawlEngine::resolve_initial_redirects`], where the crawl would stop and
/// surface a soft `state.error` rather than aborting the request.
/// Every URL the chain requests passes `policy` first, so a caller that passes `Some(policy)`
/// cannot reach a URL the configuration forbids, whatever order it does its own work in.
///
/// `override_user_agent` pins the agent every hop sends, ahead of whatever `policy` would have
/// picked. `scrape()` passes `None` for both, unchanged; the wasm crawl loop passes
/// `Some(agent)` with no policy, so the one pick its own per-page robots check already made is
/// the one that reaches the wire here too (crawlberg#483) -- native's own frontier loop still
/// picks entirely through `policy`, so passing `None` here changes nothing for it.
pub(crate) async fn follow_redirects(
    engine: &CrawlEngine,
    initial_url: &str,
    max_redirects: usize,
    mut policy: Option<&mut RedirectPolicy<'_>>,
    override_user_agent: Option<&str>,
) -> Result<RedirectResolution, CrawlError> {
    let mut chain = RedirectChain::new(initial_url, max_redirects);

    let mut browser_used = false;
    loop {
        if let Some(policy) = policy.as_deref_mut()
            && let Some(refusal) = policy.admits(&chain.current_url, chain.redirect_count > 0).await?
        {
            return Ok(RedirectResolution::Refused {
                refusal,
                redirect_count: chain.redirect_count,
                intermediate_headers: chain.intermediate_headers,
            });
        }
        // ~keep The agent `admits()` just chose (for robots.txt group selection) and this hop's
        // ~keep fetch must send are the same one: pinned onto the request below so every retry
        // ~keep or tier escalation of this hop reuses it rather than picking a new one
        // ~keep (crawlberg#423). `override_user_agent` outranks it when the caller already made
        // ~keep its own pick outside any policy (crawlberg#483); both are `None` for `scrape()`,
        // ~keep which leaves the UA rotation layer free to pick per its own default behaviour,
        // ~keep unchanged.
        let forced_user_agent = override_user_agent
            .map(str::to_owned)
            .or_else(|| policy.as_deref().and_then(|p| p.pending_user_agent.clone()));

        // ~keep Bound the read per hop: the seed's final response is now consumed directly as
        // the depth-0 page, so a document seed must be bounded here rather than in the loop.
        let hop_engine = engine.clone_for_url(&chain.current_url);
        let (resp, hop_browser_used) = match hop_engine
            .fetch_response(&chain.current_url, forced_user_agent.as_deref())
            .await
        {
            Ok(pair) => pair,
            // ~keep Redirect-chain 404s become synthetic responses so callers can inspect final_url/status_code.
            // ~keep First-hop 404 still propagates unless soft_http_errors is enabled.
            Err(CrawlError::NotFound { .. }) if chain.redirect_count > 0 => {
                return Ok(RedirectResolution::Fetched(
                    chain.into_outcome(synthetic_not_found(), browser_used),
                ));
            }
            Err(e) => return Err(e),
        };
        browser_used = hop_browser_used;

        // ~keep The browser tier follows redirects itself, so the chain learns of the hop only
        // ~keep after the request went out. Its landed URL still passes the SSRF check and the
        // ~keep policy a 3xx target does before its content is used, and a refusal discards it.
        if let Some((landed, landed_key)) = landed_redirect(&resp, &chain) {
            chain
                .advance_to(landed, landed_key, HashMap::new(), &engine.config.ssrf)
                .await?;
            if let Some(policy) = policy.as_deref_mut()
                && let Some(refusal) = policy.admits(&chain.current_url, true).await?
            {
                return Ok(RedirectResolution::Refused {
                    refusal,
                    redirect_count: chain.redirect_count,
                    intermediate_headers: chain.intermediate_headers,
                });
            }
        }

        let Some((target, target_key)) = next_redirect_target(&resp, &chain, max_redirects) else {
            return Ok(RedirectResolution::Fetched(chain.into_outcome(resp, browser_used)));
        };

        chain
            .advance_to(target, target_key, resp.headers, &engine.config.ssrf)
            .await?;
    }
}

/// The mutable state of one redirect chain: where it is, where it has been, and the
/// per-hop headers it has collected on the way.
struct RedirectChain {
    current_url: String,
    seen: HashSet<String>,
    redirect_count: usize,
    intermediate_headers: Vec<(String, HashMap<String, Vec<String>>)>,
}

impl RedirectChain {
    fn new(initial_url: &str, max_redirects: usize) -> Self {
        let current_url = initial_url.to_owned();
        let mut seen = HashSet::with_capacity(max_redirects + 1);
        seen.insert(canonical_redirect_key(&current_url));
        Self {
            current_url,
            seen,
            redirect_count: 0,
            intermediate_headers: Vec::new(),
        }
    }

    /// The cycle-detection key `target` would occupy, or `None` if the chain already used it.
    fn unseen_key(&self, target: &str) -> Option<String> {
        let key = canonical_redirect_key(target);
        (!self.seen.contains(&key)).then_some(key)
    }

    /// Move the chain to `target`, recording `headers` as the hop it is leaving.
    ///
    /// # Errors
    ///
    /// Returns [`CrawlError::SsrfViolation`] when `target` fails the SSRF policy, so a
    /// redirect can never reach a URL the configuration forbids.
    async fn advance_to(
        &mut self,
        target: String,
        target_key: String,
        headers: HashMap<String, Vec<String>>,
        ssrf: &SsrfPolicy,
    ) -> Result<(), CrawlError> {
        if let Ok(parsed_target) = url::Url::parse(&target)
            && let Err(e) = validate_url(&parsed_target, ssrf).await
        {
            return Err(CrawlError::ssrf_violation(target, e.to_string()));
        }
        self.intermediate_headers.push((url_host(&self.current_url), headers));
        self.seen.insert(target_key);
        self.redirect_count += 1;
        self.current_url = target;
        Ok(())
    }

    fn into_outcome(self, final_response: crate::tower::CrawlResponse, browser_used: bool) -> RedirectOutcome {
        RedirectOutcome {
            final_url: self.current_url,
            final_response,
            redirect_count: self.redirect_count,
            intermediate_headers: self.intermediate_headers,
            browser_used,
        }
    }
}

/// The response a 404 further down a redirect chain is reported as.
fn synthetic_not_found() -> crate::tower::CrawlResponse {
    crate::tower::CrawlResponse {
        status: 404,
        content_type: String::new(),
        body: String::new(),
        body_bytes: Vec::new(),
        headers: HashMap::new(),
        landed_url: None,
        sent_user_agent: None,
    }
}

/// The URL a self-redirecting fetcher landed on, when it is an unvisited web URL other than
/// the one requested, paired with the cycle key it will occupy.
fn landed_redirect(resp: &crate::tower::CrawlResponse, chain: &RedirectChain) -> Option<(String, String)> {
    let landed = resp.landed_url.as_deref()?;
    if !Url::parse(landed).is_ok_and(|parsed| is_fetchable_scheme(&parsed)) {
        return None;
    }
    chain.unseen_key(landed).map(|key| (landed.to_owned(), key))
}

/// The next unvisited URL `resp` points at, paired with the cycle key it will occupy.
///
/// ~keep The three sources are tried in order, and a target the chain has already visited
/// ~keep falls through to the next source rather than ending the chain: a `Location` that
/// ~keep loops back can still be superseded by a `Refresh` header or a meta refresh. Each
/// ~keep source runs only once the earlier ones yield nothing usable, so an ordinary 3xx
/// ~keep never pays to parse the body looking for a meta refresh.
fn next_redirect_target(
    resp: &crate::tower::CrawlResponse,
    chain: &RedirectChain,
    max_redirects: usize,
) -> Option<(String, String)> {
    if chain.redirect_count >= max_redirects {
        return None;
    }

    let sources: [fn(&crate::tower::CrawlResponse, &str) -> Option<String>; 3] =
        [http_redirect_target, refresh_header_target, meta_refresh_target];

    for source in sources {
        if let Some(target) = source(resp, &chain.current_url)
            && let Some(target_key) = chain.unseen_key(&target)
        {
            return Some((target, target_key));
        }
    }

    None
}

/// Statuses whose `Location` header this crawl follows.
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// `target` resolved against `base`, or `None` when it does not resolve or resolves to a scheme
/// the crawl cannot fetch (`mailto:`, `data:`, `file:`, `ftp:`, ...). A browser sends no request
/// for one, so it is no redirect target.
///
/// ~keep The scheme is checked on the resolved address, never on `target` itself: a relative
/// ~keep target has no scheme of its own to check before it resolves, so checking it there let a
/// ~keep target that takes a non-web scheme from what it resolves against through unchecked (#478).
fn fetchable_target(base: &str, target: &str) -> Option<String> {
    let resolved = resolve_redirect(base, target)?;
    Url::parse(&resolved)
        .is_ok_and(|parsed| is_fetchable_scheme(&parsed))
        .then_some(resolved)
}

/// The `Location` target of an HTTP 3xx, resolved against `current_url`, if the crawl can fetch it.
fn http_redirect_target(resp: &crate::tower::CrawlResponse, current_url: &str) -> Option<String> {
    if !REDIRECT_STATUSES.contains(&resp.status) {
        return None;
    }
    let location = resp.headers.get("location").and_then(|v| v.first())?;
    fetchable_target(current_url, location)
}

/// The target named by a `Refresh` response header, resolved against `current_url`.
fn refresh_header_target(resp: &crate::tower::CrawlResponse, current_url: &str) -> Option<String> {
    let refresh = resp.headers.get("refresh").and_then(|v| v.first())?;
    let target = refresh_target(refresh)?;
    resolve_redirect(current_url, &target)
}

/// The target named by a `<meta http-equiv="refresh">`, resolved against the document's base URL
/// (its `<base href>`, from [`effective_base_url`], the same base every other consumer uses), if
/// the crawl can fetch it. The `Refresh` header has no document to carry a base, so it resolves
/// against the response's own address instead (see [`refresh_header_target`]).
fn meta_refresh_target(resp: &crate::tower::CrawlResponse, current_url: &str) -> Option<String> {
    if !is_html_content(&resp.content_type, &resp.body) {
        return None;
    }
    // ~keep A `<meta http-equiv="refresh">` written inside script or style text is not a
    // ~keep redirect a browser would follow, so mask raw text before looking for one.
    let parsed_html = mask_raw_text_markup(&resp.body);
    let doc = crate::html::parse_html(&parsed_html).ok()?;
    let target = detect_meta_refresh(&doc)?;
    let base = Url::parse(current_url)
        .map(|document_url| effective_base_url(&doc, &document_url).to_string())
        .unwrap_or_else(|_| current_url.to_owned());
    fetchable_target(&base, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_REDIRECTS: usize = 5;

    /// ~keep A public crawl refuses a seed that does not parse before this policy runs, so the
    /// ~keep crawl-level test cannot reach the unparseable branch. Both branches are driven here.
    #[tokio::test]
    async fn a_refused_address_is_named_through_the_redactor() {
        let config = crate::CrawlConfig::builder().allow_private_networks(false).build();
        let engine = CrawlEngine::builder().config(config).build().expect("engine builds");
        let client = crate::http::build_client(&engine.config).expect("client builds");
        let mut policy = RedirectPolicy::new(&engine, &client, &[], &[]);
        for (url, expected) in [
            (
                "alice@example.com",
                "robots_unreachable: cannot parse [address hidden: it may carry credentials] to determine its origin",
            ),
            (
                "user:token@host",
                "robots_unreachable: [address hidden: it may carry credentials] has no host to read robots.txt from",
            ),
        ] {
            let Ok(Some(PolicyRefusal::Blocked { reason, .. })) = policy.admits(url, false).await else {
                panic!("{url} must be refused as blocked");
            };
            assert_eq!(reason, expected, "the refusal of {url} must not show its credential");
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> crate::tower::CrawlResponse {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (name, value) in headers {
            map.entry((*name).to_owned()).or_default().push((*value).to_owned());
        }
        crate::tower::CrawlResponse {
            status,
            content_type: if body.is_empty() {
                String::new()
            } else {
                "text/html".to_owned()
            },
            body: body.to_owned(),
            body_bytes: body.as_bytes().to_vec(),
            headers: map,
            landed_url: None,
            sent_user_agent: None,
        }
    }

    fn chain_at(current: &str, already_seen: &[&str]) -> RedirectChain {
        let mut chain = RedirectChain::new(current, MAX_REDIRECTS);
        for url in already_seen {
            chain.seen.insert(canonical_redirect_key(url));
        }
        chain
    }

    #[test]
    fn location_header_is_the_first_redirect_source_consulted() {
        let resp = response(
            302,
            &[("location", "/from-location"), ("refresh", "0; url=/from-refresh")],
            "",
        );
        let chain = chain_at("https://example.com/start", &[]);

        let (target, _) = next_redirect_target(&resp, &chain, MAX_REDIRECTS).expect("a 3xx must redirect");
        assert_eq!(target, "https://example.com/from-location");
    }

    /// Characterization: a `Location` pointing back at a URL the chain already visited does
    /// NOT end the chain — it falls through to the next redirect source on the same
    /// response. Collapsing the three sources into a first-match-wins chain silently drops
    /// this. ~keep
    #[test]
    fn a_looping_location_falls_through_to_the_refresh_header() {
        let resp = response(302, &[("location", "/start"), ("refresh", "0; url=/from-refresh")], "");
        let chain = chain_at("https://example.com/start", &[]);

        let (target, _) =
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).expect("the refresh header must still be consulted");
        assert_eq!(target, "https://example.com/from-refresh");
    }

    /// A `Location` with a scheme the crawl cannot fetch is no target, so it falls through like
    /// a looping one: to the refresh header when there is one, and to no target when there is none.
    #[test]
    fn a_non_web_location_falls_through_to_the_refresh_header() {
        let chain = chain_at("https://example.com/start", &[]);
        let resp = response(
            302,
            &[
                ("location", "mailto:a@example.com"),
                ("refresh", "0; url=/from-refresh"),
            ],
            "",
        );
        let (target, _) =
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).expect("the refresh header must still be consulted");
        assert_eq!(target, "https://example.com/from-refresh");

        let resp = response(302, &[("location", "mailto:a@example.com")], "");
        assert!(next_redirect_target(&resp, &chain, MAX_REDIRECTS).is_none());
    }

    /// A browser that lands on a page it made itself (`about:blank`, its error page) or on a
    /// non-web address has not landed on a redirect target; a web URL it landed on is one.
    #[test]
    fn only_a_web_url_a_browser_landed_on_is_a_redirect() {
        let chain = chain_at("https://example.com/start", &[]);
        let landed_on = |url: &str| {
            let mut resp = response(200, &[], "");
            resp.landed_url = Some(url.to_owned());
            landed_redirect(&resp, &chain).map(|(target, _)| target)
        };
        for url in [
            "about:blank",
            "chrome-error://chromewebdata/",
            "mailto:a@example.com",
            "data:,x",
        ] {
            assert_eq!(landed_on(url), None, "{url}");
        }
        assert_eq!(
            landed_on("https://example.com/landed").as_deref(),
            Some("https://example.com/landed")
        );
    }

    /// The same fall-through, one source further: both header sources loop, so the meta
    /// refresh in the body decides. ~keep
    #[test]
    fn a_looping_refresh_header_falls_through_to_the_meta_refresh() {
        let resp = response(
            302,
            &[("location", "/start"), ("refresh", "0; url=/seen-already")],
            r#"<html><head><meta http-equiv="refresh" content="0; url=/from-meta"></head></html>"#,
        );
        let chain = chain_at("https://example.com/start", &["https://example.com/seen-already"]);

        let (target, _) =
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).expect("the meta refresh must still be consulted");
        assert_eq!(target, "https://example.com/from-meta");
    }

    /// A `Refresh` header whose delay is a lone `.`, with no digit anywhere in it, names no
    /// refresh: the shared refresh parser rejects it exactly as it does for the meta tag, so it
    /// falls through to the meta refresh in the body (oracle case `d06_dot_only_then_longer`,
    /// #353).
    #[test]
    fn a_refresh_header_with_a_dot_only_delay_falls_through_to_the_meta_refresh() {
        let resp = response(
            200,
            &[("refresh", ".; url=/from-header")],
            r#"<html><head><meta http-equiv="refresh" content="3; url=/from-meta"></head></html>"#,
        );
        let chain = chain_at("https://example.com/start", &[]);

        let (target, _) =
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).expect("the meta refresh must still be consulted");
        assert_eq!(target, "https://example.com/from-meta");
    }

    /// A `<meta http-equiv="refresh">` written inside script text is script source, not a
    /// redirect a browser would follow. ~keep
    #[test]
    fn a_meta_refresh_inside_script_text_is_not_a_redirect() {
        let resp = response(
            200,
            &[],
            r#"<html><head><script>document.write('<meta http-equiv="refresh" content="0; url=/from-script">');</script></head></html>"#,
        );

        assert!(
            meta_refresh_target(&resp, "https://example.com/start").is_none(),
            "a meta refresh inside script text must not be followed"
        );
    }

    /// The masking pass must not stop a real meta refresh that follows script text. ~keep
    #[test]
    fn a_meta_refresh_after_script_text_is_still_followed() {
        let resp = response(
            200,
            &[],
            r#"<html><head><script>var s = "<!--";</script><meta http-equiv="refresh" content="0; url=/real"></head></html>"#,
        );

        assert_eq!(
            meta_refresh_target(&resp, "https://example.com/start"),
            Some("https://example.com/real".to_owned()),
            "a real meta refresh after a script must still be found"
        );
    }

    /// A relative meta refresh target is checked for scheme AFTER it resolves, not before: it
    /// takes `current_url`'s scheme, and a `current_url` with a scheme the crawl cannot fetch
    /// makes the resolved target one too, so it is no redirect target (#478).
    #[test]
    fn a_relative_meta_refresh_is_no_target_when_it_resolves_to_a_scheme_it_cannot_fetch() {
        let resp = response(
            200,
            &[],
            r#"<html><head><meta http-equiv="refresh" content="0; url=next"></head></html>"#,
        );

        assert_eq!(
            meta_refresh_target(&resp, "ftp://files.example/start"),
            None,
            "a relative target under a non-web current_url must not be treated as a redirect"
        );
    }

    /// The meta refresh target loses only what the URL parser strips: a no-break space stays,
    /// and a target of only C0 controls is no target. ~keep
    #[test]
    fn a_meta_refresh_target_keeps_unicode_spaces_and_drops_c0_controls() {
        let meta = |content: &str| {
            response(
                200,
                &[],
                &format!("<html><head><meta http-equiv=\"refresh\" content=\"{content}\"></head></html>"),
            )
        };
        assert_eq!(
            meta_refresh_target(&meta("0; url= /next\u{A0}"), "https://example.com/start"),
            Some("https://example.com/next%C2%A0".to_owned())
        );
        assert_eq!(
            meta_refresh_target(&meta("0; url=\u{1}\u{B}"), "https://example.com/start"),
            None
        );
    }

    /// A meta refresh target resolves against the document's base URL, exactly as a browser
    /// does: a `<base href="/app/">` sends a relative target under `/app/`, not under the page's
    /// own path (#300, matched against Chrome).
    #[test]
    fn a_meta_refresh_target_resolves_against_the_base_element() {
        let resp = response(
            200,
            &[],
            r#"<html><head><base href="/app/"><meta http-equiv="refresh" content="0; url=next"></head></html>"#,
        );
        assert_eq!(
            meta_refresh_target(&resp, "https://example.com/dir/page"),
            Some("https://example.com/app/next".to_owned()),
            "the target must resolve against the base element, not the page's own directory"
        );
    }

    /// With no `<base>` element, the page address is the base, as it always was (#300).
    #[test]
    fn a_meta_refresh_target_resolves_against_the_page_url_without_a_base_element() {
        let resp = response(
            200,
            &[],
            r#"<html><head><meta http-equiv="refresh" content="0; url=next"></head></html>"#,
        );
        assert_eq!(
            meta_refresh_target(&resp, "https://example.com/dir/page"),
            Some("https://example.com/dir/next".to_owned())
        );
    }

    /// The `Refresh` HTTP header arrives before any document exists to carry a `<base>`, so it
    /// has none to honour: it resolves against the response's own address even when the body
    /// that follows declares a base element (#300, matched against Chrome).
    #[test]
    fn a_refresh_header_target_ignores_the_bodys_base_element() {
        let resp = response(
            200,
            &[("refresh", "0; url=next")],
            r#"<html><head><base href="/app/"></head></html>"#,
        );
        assert_eq!(
            refresh_header_target(&resp, "https://example.com/dir/page"),
            Some("https://example.com/dir/next".to_owned()),
            "the Refresh header must resolve against the response URL, never the body's base element"
        );
    }

    /// The `Refresh` header target is cleaned by the URL rule, as the meta refresh target is: a
    /// no-break space stays (#206). ~keep
    #[test]
    fn a_refresh_header_target_keeps_unicode_spaces_and_drops_c0_controls() {
        let header = |value: &str| response(200, &[("refresh", value)], "");
        assert_eq!(
            refresh_header_target(&header("0; url= /next\u{A0}"), "https://example.com/start"),
            Some("https://example.com/next%C2%A0".to_owned())
        );
        assert_eq!(
            refresh_header_target(&header("0; url=\u{1}\u{B}"), "https://example.com/start"),
            None
        );
    }

    /// Both refresh forms drop one pair of matching quotes around the target (#208). ~keep
    #[test]
    fn a_quoted_refresh_target_is_followed_without_its_quotes() {
        let header = |value: &str| response(200, &[("refresh", value)], "");
        assert_eq!(
            refresh_header_target(&header("0; url='/next'"), "https://example.com/start"),
            Some("https://example.com/next".to_owned())
        );
        assert_eq!(
            refresh_header_target(&header("0; URL=\"/next\""), "https://example.com/start"),
            Some("https://example.com/next".to_owned())
        );
        let meta = response(
            200,
            &[],
            r#"<html><head><meta http-equiv="refresh" content="0; url='/next'"></head></html>"#,
        );
        assert_eq!(
            meta_refresh_target(&meta, "https://example.com/start"),
            Some("https://example.com/next".to_owned())
        );
    }

    /// The refresh header is read as a browser reads it: the label is optional, a `url=` inside
    /// the address is not a label, and a value with no leading delay is no refresh. ~keep
    #[test]
    fn a_refresh_header_is_read_with_the_browser_refresh_steps() {
        let header = |value: &str| response(200, &[("refresh", value)], "");
        assert_eq!(
            refresh_header_target(&header("0; /next"), "https://example.com/start"),
            Some("https://example.com/next".to_owned())
        );
        assert_eq!(
            refresh_header_target(&header("0; /go?url=/elsewhere"), "https://example.com/start"),
            Some("https://example.com/go?url=/elsewhere".to_owned())
        );
        assert_eq!(
            refresh_header_target(&header("url=/next"), "https://example.com/start"),
            None
        );
    }

    #[test]
    fn every_source_looping_back_ends_the_chain() {
        let resp = response(302, &[("location", "/start")], "");
        let chain = chain_at("https://example.com/start", &[]);

        assert!(
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).is_none(),
            "a chain with nowhere new to go must stop"
        );
    }

    #[test]
    fn the_hop_budget_stops_the_chain_before_any_source_is_read() {
        let resp = response(302, &[("location", "/next")], "");
        let mut chain = chain_at("https://example.com/start", &[]);
        chain.redirect_count = MAX_REDIRECTS;

        assert!(
            next_redirect_target(&resp, &chain, MAX_REDIRECTS).is_none(),
            "no further hop is allowed once max_redirects is reached"
        );
    }

    /// The hop the chain takes from a page whose head holds `contents` as meta refresh tags, in order,
    /// and whose `Refresh` header is `header`.
    fn refresh_hop(header: Option<&str>, contents: &[&str]) -> Option<String> {
        let metas: String = contents
            .iter()
            .map(|content| format!("<meta http-equiv=\"refresh\" content=\"{content}\">"))
            .collect();
        let headers: Vec<(&str, &str)> = header.map(|value| ("refresh", value)).into_iter().collect();
        let resp = response(
            200,
            &headers,
            &format!("<html><head>{metas}</head><body>x</body></html>"),
        );
        let chain = chain_at("https://example.com/start", &[]);
        next_redirect_target(&resp, &chain, MAX_REDIRECTS).map(|(target, _)| target)
    }

    /// Chrome acts on the meta refresh with the shortest delay, and on the later tag when two
    /// delays tie (#279). ~keep
    #[test]
    fn the_meta_refresh_with_the_shortest_delay_wins_and_a_tie_goes_to_the_later_tag() {
        let second = Some("https://example.com/second".to_owned());
        assert_eq!(refresh_hop(None, &["0; url=/first", "0; url=/second"]), second);
        assert_eq!(refresh_hop(None, &["3; url=/first", "0; url=/second"]), second);
        assert_eq!(refresh_hop(None, &["1; url=/first", "1.5; url=/second"]), second);
        assert_eq!(
            refresh_hop(None, &["0; url=/first", "3; url=/second"]),
            Some("https://example.com/first".to_owned())
        );
        assert_eq!(
            refresh_hop(None, &["2; url=/first", "0; url=/second", "1; url=/third"]),
            second
        );
    }

    /// A meta refresh with a blank or self target reloads the page, and a later tag with a
    /// longer delay does not replace it: the chain stays where it is (#279). ~keep
    #[test]
    fn a_meta_refresh_of_the_same_page_is_not_skipped_for_a_later_longer_one() {
        for first in ["0; url=", "0", "0;", "0; url=''", "0; url=/start"] {
            assert_eq!(
                refresh_hop(None, &[first, "3; url=/second"]),
                None,
                "{first:?} reloads the page, so the later refresh must not be followed"
            );
        }
        assert_eq!(
            refresh_hop(None, &["0; url=", "0; url=/second"]),
            Some("https://example.com/second".to_owned()),
            "a later refresh with the same delay replaces the reload"
        );
    }

    /// A `javascript:` refresh target is no refresh at all: the next meta refresh is used whatever
    /// its delay, and a lone one leaves the chain where it is (#279). ~keep
    #[test]
    fn a_javascript_meta_refresh_is_ignored() {
        for first in [
            "0; url=javascript:void(0)",
            "0; url=JavaScript:void(0)",
            "0; url= \tjavascript:void(0)",
            "0; url=java&#9;script:void(0)",
        ] {
            assert_eq!(
                refresh_hop(None, &[first, "3; url=/second"]),
                Some("https://example.com/second".to_owned()),
                "{first:?} must be ignored"
            );
        }
        assert_eq!(refresh_hop(None, &["0; url=javascript:void(0)"]), None);
    }

    /// A `javascript:` `Refresh` header is ignored rather than followed into the SSRF scheme check,
    /// and the meta refresh in the body is used (#279). ~keep
    #[test]
    fn a_javascript_refresh_header_is_ignored() {
        assert_eq!(refresh_hop(Some("0; url=javascript:void(0)"), &[]), None);
        assert_eq!(
            refresh_hop(Some("0; url=javascript:void(0)"), &["3; url=/second"]),
            Some("https://example.com/second".to_owned())
        );
    }

    /// A refresh to a scheme the crawl cannot follow keeps the page, and it still competes by delay
    /// as Chrome schedules it: a web refresh with a longer delay, before or after it, is not
    /// followed, while a later web refresh with the same delay replaces it. ~keep
    #[test]
    fn a_non_web_meta_refresh_keeps_the_page_and_still_competes_by_delay() {
        for (non_web, web) in [
            ("mailto:a@example.com", "/second"),
            ("data:text/html,x", "https://example.com/second"),
        ] {
            let non_web = format!("0; url={non_web}");
            let web = format!("3; url={web}");
            assert_eq!(refresh_hop(None, &[&non_web, &web]), None, "{non_web:?} then {web:?}");
            assert_eq!(refresh_hop(None, &[&web, &non_web]), None, "{web:?} then {non_web:?}");
            assert_eq!(
                refresh_hop(None, &[&non_web, "0; url=/second"]),
                Some("https://example.com/second".to_owned()),
                "a later web refresh with the same delay replaces {non_web:?}"
            );
        }
    }
}
