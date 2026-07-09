//! Core crawl loop implementation.
//!
//! This module contains the internal crawl orchestration logic used by
//! [`CrawlEngine::crawl`] and [`CrawlEngine::crawl_stream`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use tl::ParserOptions;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

use std::collections::HashMap;

use opentelemetry::KeyValue;

use crate::error::CrawlError;
use crate::helpers::{compile_regexes, fetch_robots_rules, find_ascii_case_insensitive};
use crate::html::{
    HtmlExtraction, detect_charset, detect_meta_refresh, extract_page_data, is_binary_content_type, is_binary_url,
    is_html_content, is_pdf_content, is_pdf_url,
};
use crate::http::{build_client, extract_cookies_from_hashmap};
use crate::net::ssrf::validate_url;
use crate::normalize::{normalize_url, normalize_url_for_dedup, resolve_redirect, strip_fragment};
use crate::robots::{RobotsRules, is_path_allowed};
use crate::telemetry::attributes::{
    CRAWL_ALLOWED, CRAWL_BROWSER_MODE, CRAWL_DEPTH, CRAWL_FRONTIER_SIZE, CRAWL_HOST, CRAWL_LINK_TYPE, CRAWL_MAX_DEPTH,
    CRAWL_MAX_PAGES, CRAWL_PAGES_COMPLETED, CRAWL_PARENT_URL, CRAWL_SEED_COUNT, CRAWL_STRATEGY, URL_DOMAIN, URL_FULL,
};
use crate::telemetry::metrics::registry;
use crate::traits::*;
use crate::types::*;

use super::CrawlEngine;

/// Map [`BrowserMode`] to a stable string label for telemetry.
fn browser_mode_label(mode: &BrowserMode) -> &'static str {
    match mode {
        BrowserMode::Auto => "auto",
        BrowserMode::Always => "always",
        BrowserMode::Never => "never",
        BrowserMode::Stealth => "stealth",
    }
}

/// Map [`EscalationStrategy`] to a stable string label for telemetry.
fn escalation_strategy_label(strategy: EscalationStrategy) -> &'static str {
    match strategy {
        EscalationStrategy::None => "none",
        EscalationStrategy::BrowserOnly => "browser_only",
        EscalationStrategy::BypassFirst => "bypass_first",
        EscalationStrategy::BypassOnly => "bypass_only",
        EscalationStrategy::BypassThenBrowser => "bypass_then_browser",
    }
}

/// Default concurrency limit when `max_concurrent` is not set.
const DEFAULT_MAX_CONCURRENT: usize = 10;

/// Outcome of a [`follow_redirects`] call.
pub(crate) struct RedirectOutcome {
    /// The final URL after all redirects have been followed.
    pub(crate) final_url: String,
    /// The HTTP response at the final URL.
    pub(crate) final_response: crate::tower::CrawlResponse,
    /// Number of redirect hops taken.
    pub(crate) redirect_count: usize,
    /// Response headers from each intermediate redirect hop (for cookie extraction).
    pub(crate) intermediate_headers: Vec<HashMap<String, Vec<String>>>,
    /// Whether headless-browser fetch was used for the final hop.
    pub(crate) browser_used: bool,
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
pub(crate) async fn follow_redirects(
    engine: &CrawlEngine,
    initial_url: &str,
    max_redirects: usize,
) -> Result<RedirectOutcome, CrawlError> {
    let mut current_url = initial_url.to_owned();
    let mut seen: HashSet<String> = HashSet::with_capacity(max_redirects + 1);
    seen.insert(current_url.clone());
    let mut redirect_count: usize = 0;
    let mut intermediate_headers: Vec<HashMap<String, Vec<String>>> = Vec::new();

    let mut browser_used = false;
    loop {
        let (resp, hop_browser_used) = match engine.fetch_response(&current_url).await {
            Ok(pair) => pair,
            // ~keep Redirect-chain 404s become synthetic responses so callers can inspect final_url/status_code.
            // ~keep First-hop 404 still propagates unless soft_http_errors is enabled.
            Err(CrawlError::NotFound(_)) if redirect_count > 0 => {
                let synthetic = crate::tower::CrawlResponse {
                    status: 404,
                    content_type: String::new(),
                    body: String::new(),
                    body_bytes: Vec::new(),
                    headers: HashMap::new(),
                };
                return Ok(RedirectOutcome {
                    final_url: current_url,
                    final_response: synthetic,
                    redirect_count,
                    intermediate_headers,
                    browser_used,
                });
            }
            Err(e) => return Err(e),
        };
        browser_used = hop_browser_used;
        let status = resp.status;

        if matches!(status, 301 | 302 | 303 | 307 | 308)
            && redirect_count < max_redirects
            && let Some(location) = resp.headers.get("location").and_then(|v| v.first())
        {
            let target = resolve_redirect(&current_url, location);
            if !seen.contains(&target) {
                if let Ok(parsed_target) = url::Url::parse(&target)
                    && let Err(e) = validate_url(&parsed_target, &engine.config.ssrf).await
                {
                    return Err(CrawlError::SsrfPolicyViolation {
                        url: target,
                        reason: e.to_string(),
                    });
                }
                intermediate_headers.push(resp.headers);
                seen.insert(target.clone());
                redirect_count += 1;
                current_url = target;
                continue;
            }
        }

        if redirect_count < max_redirects
            && let Some(refresh) = resp.headers.get("refresh").and_then(|v| v.first())
            && let Some(pos) = find_ascii_case_insensitive(refresh, "url=")
        {
            let target_path = refresh[pos + 4..].trim();
            let target = resolve_redirect(&current_url, target_path);
            if !seen.contains(&target) {
                if let Ok(parsed_target) = url::Url::parse(&target)
                    && let Err(e) = validate_url(&parsed_target, &engine.config.ssrf).await
                {
                    return Err(CrawlError::SsrfPolicyViolation {
                        url: target,
                        reason: e.to_string(),
                    });
                }
                intermediate_headers.push(resp.headers);
                seen.insert(target.clone());
                redirect_count += 1;
                current_url = target;
                continue;
            }
        }

        let meta_refresh_target: Option<String> =
            if redirect_count < max_redirects && is_html_content(&resp.content_type, &resp.body) {
                tl::parse(&resp.body, ParserOptions::default())
                    .ok()
                    .and_then(|doc| detect_meta_refresh(&doc))
            } else {
                None
            };
        if let Some(refresh_target) = meta_refresh_target {
            let target = resolve_redirect(&current_url, &refresh_target);
            if !seen.contains(&target) {
                if let Ok(parsed_target) = url::Url::parse(&target)
                    && let Err(e) = validate_url(&parsed_target, &engine.config.ssrf).await
                {
                    return Err(CrawlError::SsrfPolicyViolation {
                        url: target,
                        reason: e.to_string(),
                    });
                }
                intermediate_headers.push(resp.headers);
                seen.insert(target.clone());
                redirect_count += 1;
                current_url = target;
                continue;
            }
        }

        return Ok(RedirectOutcome {
            final_url: current_url,
            final_response: resp,
            redirect_count,
            intermediate_headers,
            browser_used,
        });
    }
}

/// Fallback URL used when a fetched URL fails to parse during extraction.
/// This should never happen in practice since the URL was already fetched successfully.
static FALLBACK_URL: std::sync::LazyLock<Url> =
    std::sync::LazyLock::new(|| Url::parse("http://invalid").expect("static fallback URL"));

/// Result of a concurrent fetch task, holding everything needed to process a completed fetch.
struct FetchResult {
    entry: FrontierEntry,
    status_code: u16,
    content_type: String,
    body: String,
    /// Raw response bytes, preserved so non-HTML documents (PDF, …) can be
    /// materialized into a [`DownloadedDocument`](crate::types::DownloadedDocument).
    body_bytes: Vec<u8>,
    headers: HashMap<String, Vec<String>>,
    extraction: HtmlExtraction,
    is_binary: bool,
    is_pdf: bool,
    detected_charset: Option<String>,
    browser_used: bool,
}

/// Result of blocking HTML extraction within a fetch task.
struct PageExtraction {
    extraction: HtmlExtraction,
    is_binary: bool,
    is_pdf: bool,
    detected_charset: Option<String>,
}

/// Mutable state accumulated during a crawl.
struct CrawlState {
    pages: Vec<CrawlPageResult>,
    normalized_urls: Vec<String>,
    redirect_count: usize,
    error: Option<String>,
    was_skipped: bool,
    all_cookies: Vec<CookieInfo>,
    pages_failed: usize,
    urls_discovered: usize,
    urls_filtered: usize,
    pages_count: usize,
    is_streaming: bool,
}

impl CrawlState {
    fn new(capacity: usize, is_streaming: bool) -> Self {
        Self {
            pages: Vec::with_capacity(capacity),
            normalized_urls: Vec::with_capacity(capacity),
            redirect_count: 0,
            error: None,
            was_skipped: false,
            all_cookies: Vec::new(),
            pages_failed: 0,
            urls_discovered: 0,
            urls_filtered: 0,
            pages_count: 0,
            is_streaming,
        }
    }

    fn into_result(self, final_url: String) -> CrawlResult {
        let (pages_to_return, stayed_on_domain) = if self.is_streaming {
            (Vec::new(), true)
        } else {
            let stayed = self.pages.iter().all(|p| p.stayed_on_domain);
            (self.pages, stayed)
        };
        CrawlResult::new(
            pages_to_return,
            final_url,
            self.redirect_count,
            self.was_skipped,
            self.error,
            self.all_cookies,
            stayed_on_domain,
            self.normalized_urls,
        )
    }
}

/// Perform HTML extraction in a blocking context.
///
/// `tl::parse` borrows the input string, so this must run via `spawn_blocking`.
fn blocking_extract_page(url: &str, content_type: &str, body: &str) -> PageExtraction {
    let parsed_url = Url::parse(url).unwrap_or_else(|_| FALLBACK_URL.clone());
    let is_binary = is_binary_content_type(content_type) || is_binary_url(url);
    let is_pdf = is_pdf_content(content_type, body) || is_pdf_url(url);
    let is_html = is_html_content(content_type, body);

    let extraction = if let Ok(doc) = tl::parse(body, ParserOptions::default()) {
        extract_page_data(&doc, body, &parsed_url, is_html && !is_binary && !is_pdf, false)
    } else {
        HtmlExtraction {
            metadata: PageMetadata::default(),
            links: Vec::new(),
            images: Vec::new(),
            feeds: Vec::new(),
            json_ld: Vec::new(),
        }
    };
    let detected_charset = detect_charset(content_type, body);

    PageExtraction {
        extraction,
        is_binary,
        is_pdf,
        detected_charset,
    }
}

impl CrawlEngine {
    /// Internal crawl implementation that uses the engine's trait objects.
    ///
    /// When `tx` is `Some`, each page is sent through the channel as it is processed
    /// so that callers can consume results incrementally via [`crawl_stream`](Self::crawl_stream).
    pub(crate) async fn crawl_with_sender(
        &self,
        url: &str,
        tx: Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
    ) -> Result<CrawlResult, CrawlError> {
        let parsed_url = Url::parse(url).map_err(|e| CrawlError::Other(format!("invalid URL: {e}")))?;
        let client = build_client(&self.config)?;
        let base_host = parsed_url.host_str().unwrap_or("").to_owned();
        let base_host_suffix = format!(".{base_host}");
        let max_depth = self.config.max_depth.unwrap_or(usize::MAX);
        let max_pages = self.config.max_pages.unwrap_or(usize::MAX);
        let max_redirects = self.config.max_redirects;

        // ~keep EnteredSpan is !Send, so job-start spans must be entered and dropped before any `.await`.
        {
            let strategy = self.config.dispatch.as_ref().map(|d| d.strategy).unwrap_or_default();
            let _engine_span = tracing::info_span!(
                "crawl.engine.start",
                { CRAWL_SEED_COUNT } = 1_i64,
                { CRAWL_MAX_DEPTH } = self.config.max_depth.map(|d| d as i64).unwrap_or(-1_i64),
                { CRAWL_MAX_PAGES } = self.config.max_pages.map(|p| p as i64).unwrap_or(-1_i64),
                { CRAWL_STRATEGY } = escalation_strategy_label(strategy),
                { CRAWL_BROWSER_MODE } = browser_mode_label(&self.config.browser.mode),
            )
            .entered();
        }

        let capacity = max_pages.min(1024);
        let is_streaming = tx.is_some();
        let mut state = CrawlState::new(capacity, is_streaming);
        let start_time = Instant::now();

        let final_url = self.resolve_initial_redirects(url, max_redirects, &mut state).await;

        if let Some(ref error_msg) = state.error {
            let error_event = CrawlEvent::Error {
                url: final_url.clone(),
                error: error_msg.clone(),
            };
            if let Some(sender) = &tx {
                let _ = sender.send(error_event.clone()).await;
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(error_event).await;
            }
            let complete_event = CrawlEvent::Complete { pages_crawled: 0 };
            if let Some(sender) = &tx {
                let _ = sender.send(complete_event.clone()).await;
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(complete_event).await;
            }
            return Ok(state.into_result(final_url));
        }

        let exclude_regexes: Vec<Regex> = compile_regexes(&self.config.exclude_paths)?;
        let include_regexes: Vec<Regex> = compile_regexes(&self.config.include_paths)?;

        let robots_rules: Option<RobotsRules> = if self.config.respect_robots_txt {
            fetch_robots_rules(&final_url, &self.config, &client).await
        } else {
            None
        };

        if let Some(rules) = &robots_rules
            && let Some(delay) = rules.crawl_delay
            && let Ok(parsed) = Url::parse(&final_url)
            && let Some(domain) = parsed.host_str()
        {
            self.rate_limiter
                .set_crawl_delay(domain, Duration::from_secs(delay))
                .await?;
        }

        // ~keep `working_set` stays local: strategies need slices, frontier is shared, and the hot path avoids locks.
        let mut working_set: Vec<FrontierEntry> = Vec::new();

        let dedup_key = normalize_url_for_dedup(&final_url);
        self.frontier.mark_seen(&dedup_key).await?;
        working_set.push(FrontierEntry {
            url: final_url.clone(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        });

        self.run_crawl_loop(
            &mut state,
            &mut working_set,
            &exclude_regexes,
            &include_regexes,
            &robots_rules,
            &base_host,
            &base_host_suffix,
            max_depth,
            max_pages,
            start_time,
            &tx,
        )
        .await?;

        if state.pages.len() > max_pages {
            state.pages.truncate(max_pages);
        }

        let pages_processed = if state.is_streaming {
            state.pages_count
        } else {
            state.pages.len()
        };
        let stats = CrawlStats {
            pages_crawled: pages_processed,
            pages_failed: state.pages_failed,
            urls_discovered: state.urls_discovered,
            urls_filtered: state.urls_filtered,
            elapsed: start_time.elapsed(),
        };
        let _ = self.store.on_complete(&stats).await;
        self.event_emitter
            .on_complete(&CompleteEvent {
                pages_crawled: pages_processed,
            })
            .await;

        if let Some(sender) = tx {
            let complete_event = CrawlEvent::Complete {
                pages_crawled: pages_processed,
            };
            let _ = sender.send(complete_event.clone()).await;
            if let Some(ref sink) = self.event_sink {
                sink.emit(complete_event).await;
            }
        } else if let Some(ref sink) = self.event_sink {
            sink.emit(CrawlEvent::Complete {
                pages_crawled: pages_processed,
            })
            .await;
        }

        let mut seen_cookies: HashSet<(String, Option<String>, Option<String>)> = HashSet::new();
        state
            .all_cookies
            .retain(|c| seen_cookies.insert((c.name.clone(), c.domain.clone(), c.path.clone())));

        Ok(state.into_result(final_url))
    }

    /// Follow HTTP, Refresh header, and meta refresh redirects until a final page is reached.
    ///
    /// Delegates to [`follow_redirects`] and maps any `CrawlError` into `state.error`,
    /// preserving the original string so that callers detect errors via `state.error.is_some()`.
    async fn resolve_initial_redirects(&self, url: &str, max_redirects: usize, state: &mut CrawlState) -> String {
        match follow_redirects(self, url, max_redirects).await {
            Ok(outcome) => {
                if self.config.cookies_enabled {
                    for headers in &outcome.intermediate_headers {
                        state.all_cookies.extend(extract_cookies_from_hashmap(headers));
                    }
                    state
                        .all_cookies
                        .extend(extract_cookies_from_hashmap(&outcome.final_response.headers));
                }
                state.redirect_count = outcome.redirect_count;
                if outcome.final_response.status >= 400 && outcome.redirect_count > 0 {
                    state.error = Some(format!("HTTP {}", outcome.final_response.status));
                }
                outcome.final_url
            }
            Err(e) => {
                state.error = Some(format!("{e}"));
                url.to_owned()
            }
        }
    }

    /// Main crawl loop: spawn fetch tasks, process results, discover links.
    #[allow(clippy::too_many_arguments)]
    async fn run_crawl_loop(
        &self,
        state: &mut CrawlState,
        working_set: &mut Vec<FrontierEntry>,
        exclude_regexes: &[Regex],
        include_regexes: &[Regex],
        robots_rules: &Option<RobotsRules>,
        base_host: &str,
        base_host_suffix: &str,
        max_depth: usize,
        max_pages: usize,
        start_time: Instant,
        tx: &Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
    ) -> Result<(), CrawlError> {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut join_set: JoinSet<Result<FetchResult, (FrontierEntry, CrawlError)>> = JoinSet::new();
        let mut cancelled = false;

        while !cancelled && (!working_set.is_empty() || !join_set.is_empty()) {
            while join_set.len() < max_concurrent && !working_set.is_empty() {
                let pages_processed = if state.is_streaming {
                    state.pages_count
                } else {
                    state.pages.len()
                };
                if pages_processed + join_set.len() >= max_pages {
                    break;
                }

                let stats = CrawlStats {
                    pages_crawled: pages_processed,
                    pages_failed: state.pages_failed,
                    urls_discovered: state.urls_discovered,
                    urls_filtered: state.urls_filtered,
                    elapsed: start_time.elapsed(),
                };
                if !self.strategy.should_continue(&stats) {
                    break;
                }

                let Some(idx) = self.strategy.select_next(working_set) else {
                    break;
                };
                let entry = working_set.swap_remove(idx);

                // ~keep EnteredSpan is !Send, so dequeue spans must be entered and dropped before any `.await`.
                {
                    let _iter_span = tracing::info_span!(
                        "crawl.loop.iteration",
                        { CRAWL_DEPTH } = entry.depth as i64,
                        { CRAWL_FRONTIER_SIZE } = working_set.len() as i64,
                        { CRAWL_PAGES_COMPLETED } = state.pages.len() as i64,
                    )
                    .entered();
                }

                if !self.should_fetch_url(
                    &entry,
                    exclude_regexes,
                    include_regexes,
                    robots_rules,
                    &mut state.urls_filtered,
                ) {
                    continue;
                }

                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| CrawlError::Other("semaphore closed".into()))?;

                let engine = self.clone();

                join_set.spawn(async move {
                    let _permit = permit;

                    let (resp, browser_used) = engine
                        .fetch_response(&entry.url)
                        .await
                        .map_err(|e| (entry.clone(), e))?;

                    let status_code = resp.status;
                    let content_type = resp.content_type.clone();
                    let headers = resp.headers.clone();
                    let body = resp.body.clone();
                    let body_bytes = resp.body_bytes;

                    let url_for_extract = entry.url.clone();
                    let content_type_clone = content_type.clone();
                    let body_clone = body.clone();

                    let page_ext = tokio::task::spawn_blocking(move || {
                        blocking_extract_page(&url_for_extract, &content_type_clone, &body_clone)
                    })
                    .await
                    .map_err(|e| (entry.clone(), CrawlError::Other(format!("extraction task failed: {e}"))))?;

                    Ok(FetchResult {
                        entry,
                        status_code,
                        content_type,
                        body,
                        body_bytes,
                        headers,
                        extraction: page_ext.extraction,
                        is_binary: page_ext.is_binary,
                        is_pdf: page_ext.is_pdf,
                        detected_charset: page_ext.detected_charset,
                        browser_used,
                    })
                });
            }

            if join_set.is_empty() {
                break;
            }

            let Some(result) = join_set.join_next().await else {
                break;
            };

            match result {
                Ok(Ok(fetch)) => {
                    let should_stop = self
                        .process_fetch_result(
                            fetch,
                            state,
                            working_set,
                            base_host,
                            base_host_suffix,
                            max_depth,
                            max_pages,
                            tx,
                            &mut join_set,
                        )
                        .await?;
                    if should_stop {
                        cancelled = true;
                    }
                }
                Ok(Err((entry, error))) => {
                    state.pages_failed += 1;
                    self.event_emitter
                        .on_error(&ErrorEvent {
                            url: entry.url.clone(),
                            error: error.to_string(),
                        })
                        .await;
                    let _ = self.store.store_error(&entry.url, &error).await;
                    let error_event = CrawlEvent::Error {
                        url: entry.url.clone(),
                        error: error.to_string(),
                    };
                    if let Some(sender) = tx {
                        let _ = sender.send(error_event.clone()).await;
                    }
                    if let Some(ref sink) = self.event_sink {
                        sink.emit(error_event).await;
                    }
                }
                Err(_join_error) => {
                    state.pages_failed += 1;
                }
            }

            let pages_processed = if state.is_streaming {
                state.pages_count
            } else {
                state.pages.len()
            };
            let stats = CrawlStats {
                pages_crawled: pages_processed,
                pages_failed: state.pages_failed,
                urls_discovered: state.urls_discovered,
                urls_filtered: state.urls_filtered,
                elapsed: start_time.elapsed(),
            };
            if !self.strategy.should_continue(&stats) {
                break;
            }
        }

        Ok(())
    }

    /// Check whether a URL should be fetched based on path filters and robots.txt.
    fn should_fetch_url(
        &self,
        entry: &FrontierEntry,
        exclude_regexes: &[Regex],
        include_regexes: &[Regex],
        robots_rules: &Option<RobotsRules>,
        urls_filtered: &mut usize,
    ) -> bool {
        let page_parsed = match Url::parse(&entry.url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let path = page_parsed.path();

        if !exclude_regexes.is_empty() && exclude_regexes.iter().any(|re| re.is_match(path)) {
            *urls_filtered += 1;
            return false;
        }
        if !include_regexes.is_empty() && entry.depth > 0 && !include_regexes.iter().any(|re| re.is_match(path)) {
            *urls_filtered += 1;
            return false;
        }
        if let Some(rules) = robots_rules {
            let host = page_parsed.host_str().unwrap_or("");
            let allowed = is_path_allowed(path, rules);

            let _span = tracing::info_span!(
                "crawl.robots.check",
                { URL_DOMAIN } = host,
                { CRAWL_HOST } = host,
                { CRAWL_ALLOWED } = allowed,
            )
            .entered();

            if !allowed {
                registry()
                    .robots_blocked_total
                    .add(1, &[KeyValue::new("host", host.to_owned())]);
                *urls_filtered += 1;
                return false;
            }
        }

        true
    }

    /// Process a completed fetch: extract data, discover links, build page result.
    ///
    /// Returns `true` if the crawl should stop (max_pages reached or receiver dropped).
    #[allow(clippy::too_many_arguments)]
    async fn process_fetch_result(
        &self,
        fetch: FetchResult,
        state: &mut CrawlState,
        working_set: &mut Vec<FrontierEntry>,
        base_host: &str,
        base_host_suffix: &str,
        max_depth: usize,
        max_pages: usize,
        tx: &Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
        join_set: &mut JoinSet<Result<FetchResult, (FrontierEntry, CrawlError)>>,
    ) -> Result<bool, CrawlError> {
        let page_url = fetch.entry.url.clone();
        let depth = fetch.entry.depth;

        if fetch.status_code >= 500 {
            state.pages_failed += 1;
            let error_msg = format!("server_error: HTTP {}", fetch.status_code);
            self.event_emitter
                .on_error(&ErrorEvent {
                    url: page_url.clone(),
                    error: error_msg.clone(),
                })
                .await;
            let _ = self
                .store
                .store_error(&page_url, &CrawlError::ServerError(error_msg.clone()))
                .await;
            let error_event = CrawlEvent::Error {
                url: page_url,
                error: error_msg,
            };
            if let Some(sender) = tx {
                let _ = sender.send(error_event.clone()).await;
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(error_event).await;
            }
            return Ok(false);
        }

        if self.config.cookies_enabled {
            state.all_cookies.extend(extract_cookies_from_hashmap(&fetch.headers));
        }

        let mut body = fetch.body;

        if let Some(max_size) = self.config.max_body_size
            && body.len() > max_size
        {
            body.truncate(max_size);
        }
        let body_size = body.len();

        let page_was_skipped = fetch.is_binary || fetch.is_pdf;
        if page_was_skipped {
            state.was_skipped = true;
        }

        let page_parsed = Url::parse(&page_url).unwrap_or_else(|_| FALLBACK_URL.clone());
        let domain = page_parsed.host_str().unwrap_or("");
        let norm_url = normalize_url(&page_url);
        let stayed_on_domain = domain == base_host;

        state.normalized_urls.push(norm_url.clone());

        let in_document_context = fetch.entry.doc_depth > 0;
        let should_discover = (!page_was_skipped || in_document_context)
            && (self.config.follow_document_urls || !in_document_context)
            && depth < max_depth;
        if should_discover {
            self.discover_and_enqueue_links(
                &fetch.extraction.links,
                &page_url,
                depth,
                fetch.entry.doc_depth,
                base_host,
                base_host_suffix,
                working_set,
                &mut state.urls_discovered,
            )
            .await?;
        }

        let downloaded_document = crate::document::build_downloaded_document(
            &page_url,
            &page_parsed,
            &fetch.content_type,
            &fetch.body_bytes,
            page_was_skipped,
            &self.config,
        );

        let markdown = if page_was_skipped {
            None
        } else {
            crate::markdown::convert_to_markdown(&body, &self.config.content).await
        };

        let page = CrawlPageResult {
            url: page_url.clone(),
            normalized_url: norm_url,
            status_code: fetch.status_code,
            content_type: fetch.content_type,
            html: body,
            body_size,
            metadata: fetch.extraction.metadata,
            links: fetch.extraction.links,
            images: fetch.extraction.images,
            feeds: fetch.extraction.feeds,
            json_ld: fetch.extraction.json_ld,
            depth,
            stayed_on_domain,
            was_skipped: page_was_skipped,
            is_pdf: fetch.is_pdf,
            detected_charset: fetch.detected_charset,
            markdown,
            extracted_data: None,
            extraction_meta: None,
            downloaded_document,
            browser_used: fetch.browser_used,
        };

        let page = match self.content_filter.filter(page).await? {
            Some(filtered_page) => filtered_page,
            None => {
                state.urls_filtered += 1;
                return Ok(false);
            }
        };

        self.strategy.on_page_processed(&page);
        let _ = self.store.store_crawl_page(&page.url, &page).await;

        self.event_emitter
            .on_page(&PageEvent {
                url: page.url.clone(),
                status_code: page.status_code,
                depth: page.depth,
            })
            .await;

        if let Some(sender) = tx {
            let page_event = CrawlEvent::Page { result: Box::new(page) };
            if sender.send(page_event.clone()).await.is_err() {
                return Ok(true);
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(page_event).await;
            }
            state.pages_count += 1;
            if state.pages_count >= max_pages {
                join_set.abort_all();
                return Ok(true);
            }
        } else {
            let page_event = CrawlEvent::Page {
                result: Box::new(page.clone()),
            };
            if let Some(ref sink) = self.event_sink {
                sink.emit(page_event).await;
            }
            state.pages.push(page);
            if state.pages.len() >= max_pages {
                join_set.abort_all();
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Discover links from a page and add unseen ones to the working set.
    ///
    /// `parent_doc_depth` is taken from `entry.doc_depth` of the page being processed.
    /// It is 0 for pages reached via ordinary HTML navigation, and > 0 for pages reached
    /// via consecutive `LinkType::Document` hops.
    ///
    /// Called only when the caller has already determined that discovery is appropriate
    /// (i.e. `follow_document_urls` is satisfied for in-document-context pages).
    ///
    /// `LinkType::Internal` links are always enqueued.
    /// `LinkType::Document` links are enqueued when either:
    ///   * parent_doc_depth == 0 (HTML page discovering document URLs — original behaviour),
    ///   * OR `follow_document_urls` is true AND the child doc_depth does not exceed
    ///     `document_url_depth` (if set).
    ///
    /// SSRF validation is applied at enqueue time with bounded concurrency (16 concurrent
    /// DNS lookups). URLs that fail validation are logged as warnings and not enqueued.
    #[allow(clippy::too_many_arguments)]
    async fn discover_and_enqueue_links(
        &self,
        links: &[LinkInfo],
        _page_url: &str,
        depth: usize,
        parent_doc_depth: u32,
        base_host: &str,
        base_host_suffix: &str,
        working_set: &mut Vec<FrontierEntry>,
        urls_discovered: &mut usize,
    ) -> Result<(), CrawlError> {
        let mut candidates = Vec::new();

        for link in links {
            let is_doc_link = link.link_type == LinkType::Document;

            if link.link_type != LinkType::Internal && !is_doc_link {
                continue;
            }

            // ~keep Document pages can discover more documents only within follow_document_urls/depth policy.
            if is_doc_link && parent_doc_depth > 0 {
                if !self.config.follow_document_urls {
                    continue;
                }
                let child_doc_depth = parent_doc_depth + 1;
                if let Some(max_doc_depth) = self.config.document_url_depth
                    && child_doc_depth > max_doc_depth
                {
                    continue;
                }
            }

            let link_url = strip_fragment(&link.url);

            if self.config.stay_on_domain
                && let Ok(lu) = Url::parse(&link_url)
            {
                let link_host = lu.host_str().unwrap_or("");
                if link_host != base_host && (!self.config.allow_subdomains || !link_host.ends_with(base_host_suffix)) {
                    continue;
                }
            }

            let child_depth = depth + 1;
            let dedup_key = normalize_url_for_dedup(&link_url);
            // ~keep Mark seen before SSRF validation so concurrent discovery cannot enqueue dedup-equivalent URLs.
            if !self.frontier.is_seen(&dedup_key).await? {
                self.frontier.mark_seen(&dedup_key).await?;
                candidates.push((link_url, is_doc_link, child_depth));
            }
        }

        const SSRF_VALIDATION_CONCURRENCY: usize = 16;
        let semaphore = Arc::new(Semaphore::new(SSRF_VALIDATION_CONCURRENCY));
        let mut join_set = JoinSet::new();

        for (link_url, is_doc_link, child_depth) in candidates {
            let dedup_key = normalize_url_for_dedup(&link_url);
            let permit = Arc::clone(&semaphore);
            let ssrf_policy = self.config.ssrf.clone();

            join_set.spawn(async move {
                let _permit = permit.acquire().await.ok();
                let url_obj = match url::Url::parse(&link_url) {
                    Ok(u) => u,
                    Err(_) => return Err((link_url.clone(), "invalid URL format".to_string())),
                };

                match validate_url(&url_obj, &ssrf_policy).await {
                    Ok(_) => Ok((link_url, dedup_key, is_doc_link, child_depth)),
                    Err(e) => Err((link_url, e.to_string())),
                }
            });
        }

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok((link_url, _dedup_key, is_doc_link, child_depth))) => {
                    let child_doc_depth: u32 = if is_doc_link { parent_doc_depth + 1 } else { 0 };
                    let priority = self.strategy.score_url(&link_url, child_depth);

                    {
                        let link_host = Url::parse(&link_url)
                            .ok()
                            .and_then(|u| u.host_str().map(str::to_owned))
                            .unwrap_or_default();
                        let _discover_span = tracing::info_span!(
                            "crawl.page.discover",
                            { URL_FULL } = %link_url,
                            { URL_DOMAIN } = %link_host,
                            { CRAWL_PARENT_URL } = %_page_url,
                            { CRAWL_DEPTH } = child_depth as i64,
                            { CRAWL_LINK_TYPE } = if is_doc_link { "document" } else { "internal" },
                        )
                        .entered();
                    }

                    working_set.push(FrontierEntry {
                        url: link_url.clone(),
                        depth: child_depth,
                        doc_depth: child_doc_depth,
                        priority,
                    });
                    *urls_discovered += 1;
                    self.event_emitter.on_discovered(&link_url, child_depth).await;
                }
                Ok(Err((link_url, reason))) => {
                    tracing::warn!(
                        url = %link_url,
                        reason = %reason,
                        "link rejected by SSRF policy at enqueue time"
                    );
                }
                Err(e) => {
                    tracing::error!("error validating link during enqueue: {}", e);
                }
            }
        }

        Ok(())
    }
}
