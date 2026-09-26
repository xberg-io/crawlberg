//! Core crawl loop implementation.
//!
//! This module contains the internal crawl orchestration logic used by
//! [`CrawlEngine::crawl`] and [`CrawlEngine::crawl_stream`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

use opentelemetry::KeyValue;

use crate::error::CrawlError;
use crate::helpers::{RobotsOutcome, compile_regexes};
use crate::html::{is_binary_url, is_pdf_url};
use crate::http::{build_client, extract_cookies_from_hashmap};
use crate::normalize::normalize_url_for_dedup;
use crate::telemetry::attributes::{
    CRAWL_ALLOWED, CRAWL_BROWSER_MODE, CRAWL_DEPTH, CRAWL_FRONTIER_SIZE, CRAWL_HOST, CRAWL_MAX_DEPTH, CRAWL_MAX_PAGES,
    CRAWL_PAGES_COMPLETED, CRAWL_SEED_COUNT, CRAWL_STRATEGY, URL_DOMAIN,
};
use crate::telemetry::metrics::registry;
use crate::traits::*;
use crate::types::*;

use super::CrawlEngine;
use super::crawl_state::{
    CrawlState, FetchOutcome, FetchResult, LoopContext, blocking_extract_page, receiver_closed, receiver_gone,
};
use super::redirect::{PolicyRefusal, RedirectOutcome, RedirectPolicy, RedirectResolution, follow_redirects, url_host};

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

/// Drop the entry for a fetch that has reported back, so only genuinely running URLs remain.
fn retire_in_flight(in_flight: &mut Vec<FrontierEntry>, url: &str) {
    if let Some(position) = in_flight.iter().position(|entry| entry.url == url) {
        in_flight.swap_remove(position);
    }
}

/// Seed-derived limits and host scope, fixed for the lifetime of one crawl.
///
/// ~keep Grouped rather than derived inline so `crawl_with_sender` stays inside the 80-line
/// function limit; these five always travel together and none of them changes once the loop
/// starts.
struct CrawlBounds {
    base_host: String,
    base_host_suffix: String,
    max_depth: usize,
    max_pages: usize,
    max_redirects: usize,
}

impl CrawlBounds {
    fn resolve(config: &CrawlConfig, seed_url: &str) -> Result<Self, CrawlError> {
        let parsed = Url::parse(seed_url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
        let base_host = parsed.host_str().unwrap_or("").to_owned();
        Ok(Self {
            base_host_suffix: format!(".{base_host}"),
            base_host,
            max_depth: config.max_depth.unwrap_or(usize::MAX),
            max_pages: config.max_pages.unwrap_or(usize::MAX),
            max_redirects: config.max_redirects,
        })
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
        let seed_url = crate::helpers::strip_seed_tracking_params(&self.config, url);
        let client = build_client(&self.config)?;
        let bounds = CrawlBounds::resolve(&self.config, &seed_url)?;

        self.emit_crawl_start_span();

        let capacity = bounds.max_pages.min(1024);
        let is_streaming = tx.is_some();
        let mut state = CrawlState::new(capacity, is_streaming);
        let start_time = Instant::now();

        // ~keep `Arc` rather than `Vec`: every spawned frontier fetch builds its own
        // ~keep task-local `RedirectPolicy` (see `fetch_and_extract`) and needs a cheap,
        // ~keep `'static` clone of this list to do it.
        let exclude_regexes: Arc<[Regex]> = compile_regexes(&self.config.exclude_paths)?.into();
        let include_regexes: Vec<Regex> = compile_regexes(&self.config.include_paths)?;

        // ~keep robots.txt is read before anything goes on the wire, and the policy travels
        // into the redirect resolution below rather than bracketing it. A redirect can leave
        // the seed's robots.txt scope (scheme, host and port), and reading the new origin's
        // file after the chain has already been fetched asks the question one request late.
        let mut policy = RedirectPolicy::new(self, &client, exclude_regexes.as_ref(), &include_regexes);
        // ~keep A stream dropped while the seed is still resolving abandons it here, so its
        // ~keep retries and redirect hops stop with it; the loop below watches the same drop.
        let seed = tokio::select! {
            biased;
            () = receiver_closed(&tx) => return Ok(self.finish_without_crawling(state, seed_url, &tx).await),
            seed = self.resolve_initial_redirects(&seed_url, bounds.max_redirects, &mut state, &mut policy) => seed,
        };
        state.urls_filtered += policy.urls_filtered;

        let seed = match seed {
            Ok(seed) => seed,
            Err(refusal) => {
                let (refused_url, reason) = refusal.into_parts();
                state.was_skipped = reason.is_some();
                state.error = reason;
                return Ok(self.finish_without_crawling(state, refused_url, &tx).await);
            }
        };
        let robots = policy.into_outcome();
        let final_url = seed
            .as_ref()
            .map(|outcome| outcome.final_url.clone())
            .unwrap_or_else(|| seed_url.clone());

        if state.error.is_some() {
            return Ok(self.finish_without_crawling(state, final_url, &tx).await);
        }
        let Some(seed) = seed else {
            return Ok(self.finish_without_crawling(state, final_url, &tx).await);
        };

        self.seed_frontier(&final_url, &mut state).await?;

        // ~keep The seed keeps flowing through the frontier and the loop so that budget,
        // streaming, max_pages and filter accounting stay in exactly one place; only its
        // *fetch* is skipped, by handing the loop the response we already have.
        let mut preloaded = Some((final_url.clone(), seed.final_response, seed.browser_used));

        let context = LoopContext {
            exclude_regexes: Arc::clone(&exclude_regexes),
            include_regexes: &include_regexes,
            robots: &robots,
            base_host: &bounds.base_host,
            base_host_suffix: &bounds.base_host_suffix,
            max_depth: bounds.max_depth,
            max_pages: bounds.max_pages,
            start_time,
            tx: &tx,
        };
        self.run_crawl_loop(&mut state, &mut preloaded, &context).await?;

        Ok(self.finish_crawl(state, final_url, &context).await)
    }

    /// ~keep EnteredSpan is !Send, so job-start spans must be entered and dropped before any `.await`.
    fn emit_crawl_start_span(&self) {
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

    /// Put the resolved seed on the frontier as the depth-0 entry, marking it seen first.
    async fn seed_frontier(&self, final_url: &str, state: &mut CrawlState) -> Result<(), CrawlError> {
        let dedup_key = normalize_url_for_dedup(final_url, self.config.dedup_include_query);
        self.frontier.mark_seen(&dedup_key).await?;
        self.push_to_frontier(
            FrontierEntry {
                url: final_url.to_owned(),
                depth: 0,
                doc_depth: 0,
                priority: 1.0,
            },
            state,
        )
        .await
    }

    /// Emit the terminal events for a completed crawl and build its result.
    async fn finish_crawl(&self, mut state: CrawlState, final_url: String, context: &LoopContext<'_>) -> CrawlResult {
        if state.pages.len() > context.max_pages {
            state.pages.truncate(context.max_pages);
        }

        let pages_processed = state.pages_processed();
        let _ = self.store.on_complete(&crawl_stats(&state, context.start_time)).await;
        self.event_emitter
            .on_complete(&CompleteEvent {
                pages_crawled: pages_processed,
            })
            .await;

        if let Some(sender) = context.tx {
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

        state.into_result(final_url)
    }

    /// Emit the terminal events for a crawl that never entered the loop, and build its result.
    ///
    /// ~keep Extracted so every pre-loop bail-out -- robots unreachable, seed disallowed,
    /// seed network failure, seed HTTP error -- reports through one path.
    async fn finish_without_crawling(
        &self,
        state: CrawlState,
        final_url: String,
        tx: &Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
    ) -> CrawlResult {
        if let Some(ref error_msg) = state.error {
            let error_event = CrawlEvent::Error {
                url: final_url.clone(),
                error: error_msg.clone(),
            };
            if let Some(sender) = tx {
                let _ = sender.send(error_event.clone()).await;
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(error_event).await;
            }
            // ~keep `EventSink` and `EventEmitter` are separate traits on separate engine
            // ~keep fields; emitting to the sink does not reach an emitter. Every bail-out
            // ~keep routed through here skipped the emitter entirely, so a consumer built on
            // ~keep callbacks saw nothing at all for a seed failure -- and since 1.6.1 this
            // ~keep path also serves "robots.txt unreachable" and "seed disallowed", so an
            // ~keep origin with a 5xx robots.txt went completely silent on that channel.
            self.event_emitter
                .on_error(&ErrorEvent {
                    url: final_url.clone(),
                    error: error_msg.clone(),
                })
                .await;
        }
        let complete_event = CrawlEvent::Complete { pages_crawled: 0 };
        if let Some(sender) = tx {
            let _ = sender.send(complete_event.clone()).await;
        }
        if let Some(ref sink) = self.event_sink {
            sink.emit(complete_event).await;
        }
        // ~keep Missing alongside `on_error`: without it a callback consumer sees neither a
        // ~keep failure nor a completion, which reads as a hung crawl rather than a failed one.
        self.event_emitter
            .on_complete(&CompleteEvent { pages_crawled: 0 })
            .await;
        state.into_result(final_url)
    }

    /// Clone this engine, lifting `max_body_size` to `document_max_size` for document-shaped URLs.
    ///
    /// ~keep `http::read_body_bounded` is the only place that bounds the network read, and it
    /// is driven by `http::effective_max_body_size`, which falls back to a 100 MiB ceiling.
    /// When `download_documents` is on (its default) and the URL looks like a document, lift
    /// the clone's `max_body_size` to `document_max_size` so a large PDF/DOCX/etc. is never
    /// fully materialized in memory. Scoped to document-shaped URLs (rather than every
    /// request) so an explicit `max_body_size` -- or a plain large HTML page -- keeps today's
    /// behavior; `self.config` is untouched.
    ///
    /// ~keep Applied to the seed's redirect chain as well as to each loop entry: the seed's
    /// response is now reused rather than refetched, so bounding it only inside the loop
    /// would leave a document seed unbounded.
    pub(super) fn clone_for_url(&self, url: &str) -> Self {
        let mut engine = self.clone();
        if engine.config.download_documents
            && engine.config.max_body_size.is_none()
            && (is_binary_url(url) || is_pdf_url(url))
        {
            engine.config.max_body_size = Some(
                engine
                    .config
                    .document_max_size
                    .unwrap_or(crate::document::DEFAULT_DOCUMENT_MAX_SIZE),
            );
        }
        engine
    }

    /// Publish any `Crawl-delay` from robots.txt to the per-domain rate limiter.
    pub(super) async fn apply_crawl_delay(&self, robots: &RobotsOutcome, parsed: &Url) -> Result<(), CrawlError> {
        if let Some(rules) = robots.rules()
            && let Some(delay) = rules.crawl_delay
            && let Some(domain) = parsed.host_str()
        {
            self.rate_limiter
                .set_crawl_delay(domain, Duration::from_secs(delay))
                .await?;
        }
        Ok(())
    }

    /// Follow HTTP, Refresh header, and meta refresh redirects until a final page is reached.
    ///
    /// Delegates to [`follow_redirects`] and maps any `CrawlError` into `state.error`,
    /// preserving the original string so that callers detect errors via `state.error.is_some()`.
    ///
    /// ~keep Returns the whole [`RedirectOutcome`], including `final_response`. It used to
    /// return only the final URL and drop the response, which is what made the crawl loop
    /// fetch the seed a second time.
    /// `Ok(None)` is a chain that failed, with `state.error` carrying the reason. `Err` is the
    /// policy refusing a URL, which stops the crawl before that URL is requested.
    async fn resolve_initial_redirects(
        &self,
        url: &str,
        max_redirects: usize,
        state: &mut CrawlState,
        policy: &mut RedirectPolicy<'_>,
    ) -> Result<Option<RedirectOutcome>, PolicyRefusal> {
        let resolution = match follow_redirects(self, url, max_redirects, Some(policy)).await {
            Ok(RedirectResolution::Refused {
                refusal,
                redirect_count,
                intermediate_headers,
            }) => {
                // ~keep A refusal still happened after real hops: keep what they produced so
                // ~keep the reported result matches what the crawl actually did.
                if self.config.cookies_enabled {
                    for (host, headers) in &intermediate_headers {
                        state.all_cookies.extend(extract_cookies_from_hashmap(host, headers));
                    }
                }
                state.redirect_count = redirect_count;
                return Err(refusal);
            }
            Ok(RedirectResolution::Fetched(outcome)) => Ok(outcome),
            Err(e) => Err(e),
        };
        Ok(match resolution {
            Ok(outcome) => {
                if self.config.cookies_enabled {
                    for (host, headers) in &outcome.intermediate_headers {
                        state.all_cookies.extend(extract_cookies_from_hashmap(host, headers));
                    }
                    let final_host = url_host(&outcome.final_url);
                    state.all_cookies.extend(extract_cookies_from_hashmap(
                        &final_host,
                        &outcome.final_response.headers,
                    ));
                }
                state.redirect_count = outcome.redirect_count;
                if outcome.final_response.status >= 400 && outcome.redirect_count > 0 {
                    state.error = Some(format!("HTTP {}", outcome.final_response.status));
                }
                Some(outcome)
            }
            Err(e) => {
                state.error = Some(format!("{e}"));
                None
            }
        })
    }

    /// Push one entry onto the frontier, wrapping any backend failure with the URL.
    pub(super) async fn push_to_frontier(
        &self,
        entry: FrontierEntry,
        state: &mut CrawlState,
    ) -> Result<(), CrawlError> {
        let url = entry.url.clone();
        self.frontier
            .push(entry)
            .await
            .map_err(|e| CrawlError::other_with_source(format!("pushing {url} onto the crawl frontier failed"), e))?;
        state.frontier_pending += 1;
        Ok(())
    }

    /// Top up `window` from the frontier.
    ///
    /// Returns `false` when the frontier yielded fewer entries than asked for, i.e. it is
    /// empty until discovery pushes again. A short batch is the emptiness signal, so the
    /// loop never has to call the async `Frontier::len`/`is_empty` on the hot path.
    async fn refill_window(
        &self,
        window: &mut Vec<FrontierEntry>,
        capacity: usize,
        state: &mut CrawlState,
    ) -> Result<bool, CrawlError> {
        let wanted = capacity.saturating_sub(window.len());
        if wanted == 0 {
            return Ok(true);
        }

        let popped = self
            .frontier
            .pop_batch(wanted)
            .await
            .map_err(|e| CrawlError::other_with_source("refilling the crawl window from the frontier failed", e))?;

        state.frontier_pending = state.frontier_pending.saturating_sub(popped.len());
        let filled = popped.len();
        window.extend(popped);
        Ok(filled == wanted)
    }

    /// Return unprocessed window entries to the frontier so a persistent or distributed
    /// frontier does not lose the work when the loop stops early.
    async fn spill_window(&self, window: &mut Vec<FrontierEntry>, state: &mut CrawlState) -> Result<(), CrawlError> {
        for entry in std::mem::take(window) {
            self.push_to_frontier(entry, state).await?;
        }
        Ok(())
    }

    /// Main crawl loop. Owns the selection window and returns it to the frontier on every
    /// exit path, including the error ones.
    async fn run_crawl_loop(
        &self,
        state: &mut CrawlState,
        preloaded: &mut Option<(String, crate::tower::CrawlResponse, bool)>,
        context: &LoopContext<'_>,
    ) -> Result<(), CrawlError> {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let mut window: Vec<FrontierEntry> = Vec::with_capacity(max_concurrent);
        let mut in_flight: Vec<FrontierEntry> = Vec::with_capacity(max_concurrent);

        let outcome = self
            .drive_crawl_loop(state, &mut window, &mut in_flight, preloaded, context)
            .await;

        // ~keep In-flight entries first: they were selected before anything left in the window,
        // so returning them ahead of it keeps the frontier's ordering closest to the order the
        // crawl would have used had it continued.
        window.splice(0..0, in_flight.drain(..));

        let spilled = self.spill_window(&mut window, state).await;
        match (outcome, spilled) {
            (Err(loop_error), Err(spill_error)) => {
                // ~keep The loop error is the cause the caller needs; the spill failure only
                // means a persistent frontier lost queued URLs. Log it rather than let it
                // mask the primary failure.
                tracing::warn!(
                    error = %spill_error,
                    "returning unprocessed frontier entries failed while the crawl was already failing"
                );
                Err(loop_error)
            }
            (Err(loop_error), Ok(())) => Err(loop_error),
            (Ok(()), spilled) => spilled,
        }
    }

    /// Drive the crawl: refill the window, spawn fetches, process results, discover links.
    async fn drive_crawl_loop(
        &self,
        state: &mut CrawlState,
        window: &mut Vec<FrontierEntry>,
        in_flight: &mut Vec<FrontierEntry>,
        preloaded: &mut Option<(String, crate::tower::CrawlResponse, bool)>,
        context: &LoopContext<'_>,
    ) -> Result<(), CrawlError> {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let mut drive = LoopDrive::new(window, in_flight, max_concurrent);
        let mut cancelled = false;

        while !cancelled && !receiver_gone(context.tx) {
            self.spawn_pending_fetches(&mut drive, state, preloaded, context)
                .await?;

            if drive.join_set.is_empty() {
                // ~keep A short `pop_batch` is the cheap emptiness signal, but a queue-backed
                // frontier may legitimately under-deliver while still holding work (SQS short
                // polling returns 0-N messages from a non-empty queue). Before concluding the
                // crawl, confirm with `is_empty`. Once per completed fetch at most, so a
                // frontier that reports non-empty but never yields cannot spin the loop.
                if drive.window.is_empty() && !drive.drain_confirmed && !self.frontier.is_empty().await? {
                    drive.drain_confirmed = true;
                    drive.frontier_may_have_entries = true;
                    continue;
                }
                break;
            }

            // ~keep A dropped receiver ends the loop here rather than at the next page send: a
            // ~keep failed fetch's error event ignores its failed send, so a run of failures kept
            // ~keep the crawl starting requests nobody would read. Leaving the loop drops `drive`,
            // ~keep whose `JoinSet` aborts the fetches still in flight, retries included.
            let joined = tokio::select! {
                biased;
                () = receiver_closed(context.tx) => break,
                joined = drive.join_set.join_next() => joined,
            };
            let Some(result) = joined else {
                break;
            };

            cancelled = self.absorb_fetch_result(result, &mut drive, state, context).await?;

            // ~keep Link discovery on the completed page may have pushed; re-arm the latch so
            // the next refill looks again. Without it a frontier that ran dry once would never
            // be polled after new work arrived.
            drive.frontier_may_have_entries = true;
            drive.drain_confirmed = false;

            if !self.strategy.should_continue(&crawl_stats(state, context.start_time)) {
                break;
            }
        }

        Ok(())
    }

    /// Start fetches until the concurrency window is full, or until nothing more may start.
    ///
    /// Returning early is how this loop signals "stop spawning" — the max-pages ceiling, a
    /// strategy that has had enough, an exhausted page budget and an empty frontier all end
    /// the spawn pass without ending the crawl: fetches already in flight still finish.
    async fn spawn_pending_fetches(
        &self,
        drive: &mut LoopDrive<'_>,
        state: &mut CrawlState,
        preloaded: &mut Option<(String, crate::tower::CrawlResponse, bool)>,
        context: &LoopContext<'_>,
    ) -> Result<(), CrawlError> {
        while drive.join_set.len() < drive.max_concurrent {
            if drive.frontier_may_have_entries && drive.window.len() < drive.max_concurrent {
                drive.frontier_may_have_entries = self.refill_window(drive.window, drive.max_concurrent, state).await?;
            }
            if drive.window.is_empty() {
                return Ok(());
            }

            if state.pages_processed() + drive.join_set.len() >= context.max_pages {
                return Ok(());
            }

            if !self.strategy.should_continue(&crawl_stats(state, context.start_time)) {
                return Ok(());
            }

            let Some((index, entry)) = super::take_selected(self.strategy.as_ref(), drive.window) else {
                return Ok(());
            };

            emit_dequeue_span(&entry, drive.window.len(), state);

            if !self.should_fetch_url(&entry, context, &mut state.urls_filtered) {
                continue;
            }

            if !self.budget_admits_another_page().await {
                // ~keep Budget exhaustion pauses the crawl, it does not reject this URL, so the
                // entry goes back where it came from and is spilled to the frontier on exit.
                // A robots/path-filtered entry above is dropped instead: that one was rejected.
                let position = index.min(drive.window.len());
                drive.window.insert(position, entry);
                return Ok(());
            }

            let permit = drive
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| CrawlError::other("semaphore closed"))?;

            // ~keep `http.rs::read_body_bounded` is the only place that bounds the network
            // read, and it is driven by `http::effective_max_body_size`, which falls back
            // to a 100 MiB ceiling. When `download_documents` is on (its default) and the
            // URL looks like a document, lift this per-task clone's `max_body_size`
            // to `document_max_size` so a large PDF/DOCX/etc. is never fully materialized
            // in memory regardless of `document_max_size`, matching how `read_body_bounded`
            // already bounds the HTML body path. Scoped to document-shaped URLs (rather than
            // every request) so an explicit `max_body_size` — or a plain large HTML page —
            // keeps today's behavior; `self.config` (used by the rest of this loop, e.g. the
            // `max_body_size` truncation in `process_fetch_result`) is untouched.
            let engine = self.clone_for_url(&entry.url);

            // ~keep The entry moves into the task, so it is unreachable if that task is
            // aborted. Keep a copy here and drop it when the fetch reports back, so an
            // early exit can return still-running URLs to the frontier instead of
            // stranding them: they were marked seen at discovery and a persistent
            // frontier would otherwise never revisit them.
            drive.in_flight.push(entry.clone());

            let preloaded_response = take_preloaded_response(preloaded, &entry.url);
            Self::spawn_fetch(drive, engine, entry, preloaded_response, permit, context);
        }

        Ok(())
    }

    /// Hand one fetch to the `JoinSet`, cloning what it needs to outlive this call.
    fn spawn_fetch(
        drive: &mut LoopDrive<'_>,
        engine: CrawlEngine,
        entry: FrontierEntry,
        preloaded_response: Option<(crate::tower::CrawlResponse, bool)>,
        permit: tokio::sync::OwnedSemaphorePermit,
        context: &LoopContext<'_>,
    ) {
        let exclude_regexes = Arc::clone(&context.exclude_regexes);
        // ~keep `LoopContext::include_regexes` is a borrowed slice, so a fresh `Arc` is built
        // ~keep here rather than cloned, unlike `exclude_regexes`: the spawned task still
        // ~keep needs an owned, `'static` list for its own task-local `RedirectPolicy`.
        let include_regexes: Arc<[Regex]> = context.include_regexes.into();
        drive.join_set.spawn(fetch_and_extract(
            engine,
            entry,
            preloaded_response,
            permit,
            exclude_regexes,
            include_regexes,
        ));
    }

    /// Whether the page budget still admits another fetch.
    ///
    /// ~keep The budget hook was previously checked only under cfg(wasm32), making it a
    /// silent no-op on every native binding. It is consulted at the point the wasm path
    /// gates: after filtering, before a permit is taken. A `false` stops spawning rather
    /// than cancelling, matching the max_pages ceiling — in-flight fetches still finish.
    async fn budget_admits_another_page(&self) -> bool {
        match self.page_budget.check().await {
            Ok(()) => true,
            Err(crate::budget::BudgetError::Exhausted) => {
                tracing::info!(target: "crawlberg.budget", "page budget exhausted");
                false
            }
            Err(crate::budget::BudgetError::Backend(message)) => {
                // ~keep WARN, not ERROR: the crawl does not fail — it stops spawning and
                // returns the pages already collected. Degraded, not lost. Matches the
                // wasm path's handling of the same condition in engine/mod.rs.
                tracing::warn!(
                    target: "crawlberg.budget",
                    error = %message,
                    "budget backend error; treating as exhausted"
                );
                false
            }
        }
    }

    /// Fold one finished fetch into the crawl, reporting whether it is the one that ends it.
    async fn absorb_fetch_result(
        &self,
        result: Result<Result<FetchOutcome, (FrontierEntry, CrawlError)>, tokio::task::JoinError>,
        drive: &mut LoopDrive<'_>,
        state: &mut CrawlState,
        context: &LoopContext<'_>,
    ) -> Result<bool, CrawlError> {
        match result {
            Ok(Ok(FetchOutcome::Fetched(fetch))) => {
                retire_in_flight(drive.in_flight, &fetch.entry.url);
                self.process_fetch_result(*fetch, state, context).await
            }
            Ok(Ok(FetchOutcome::Skipped(entry))) => {
                retire_in_flight(drive.in_flight, &entry.url);
                state.urls_filtered += 1;
                Ok(false)
            }
            Ok(Err((entry, error))) => {
                retire_in_flight(drive.in_flight, &entry.url);
                state.pages_failed += 1;
                self.report_fetch_error(&entry.url, &error, context).await;
                Ok(false)
            }
            Err(_join_error) => {
                state.pages_failed += 1;
                Ok(false)
            }
        }
    }

    /// Emit the error events for a fetch that failed before it produced a page.
    async fn report_fetch_error(&self, url: &str, error: &CrawlError, context: &LoopContext<'_>) {
        self.event_emitter
            .on_error(&ErrorEvent {
                url: url.to_owned(),
                error: error.to_string(),
            })
            .await;
        let _ = self.store.store_error(url, error).await;
        let error_event = CrawlEvent::Error {
            url: url.to_owned(),
            error: error.to_string(),
        };
        if let Some(sender) = context.tx {
            let _ = sender.send(error_event.clone()).await;
        }
        if let Some(ref sink) = self.event_sink {
            sink.emit(error_event).await;
        }
    }

    /// Check whether a URL should be fetched based on path filters and robots.txt.
    fn should_fetch_url(&self, entry: &FrontierEntry, context: &LoopContext<'_>, urls_filtered: &mut usize) -> bool {
        let exclude_regexes: &[Regex] = &context.exclude_regexes;
        let include_regexes = context.include_regexes;
        let robots = context.robots;
        let page_parsed = match Url::parse(&entry.url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let path = page_parsed.path();

        // ~keep `include_paths` is exempt at depth 0 (the seed): the seed was not discovered
        // through any filter, so requiring it to match its own include pattern would refuse
        // crawls whose seed legitimately falls outside the pattern meant for its children.
        if !crate::helpers::passes_path_patterns(
            &page_parsed,
            exclude_regexes,
            include_regexes,
            entry.depth > 0,
            self.config.path_patterns_match_query,
            urls_filtered,
        ) {
            return false;
        }
        if !matches!(robots, RobotsOutcome::AllowAll) {
            let host = page_parsed.host_str().unwrap_or("");
            let allowed = robots.allows(path);

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
}

/// The work in flight for one `drive_crawl_loop` run: what is queued, what is running,
/// and the latches that decide whether the frontier is worth asking again.
struct LoopDrive<'a> {
    /// Entries taken from the frontier but not yet spawned.
    ///
    /// ~keep Borrowed rather than owned: `run_crawl_loop` must be able to return both this
    /// ~keep and `in_flight` to the frontier on every exit path, error paths included.
    window: &'a mut Vec<FrontierEntry>,
    /// Entries whose fetch task is running, kept so an early exit can return them.
    in_flight: &'a mut Vec<FrontierEntry>,
    /// The running fetches. Dropping it, as every exit from the loop does, aborts them.
    join_set: JoinSet<Result<FetchOutcome, (FrontierEntry, CrawlError)>>,
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    /// Whether the frontier may still hold work; cleared when a refill comes up short.
    frontier_may_have_entries: bool,
    /// Whether `Frontier::is_empty` has already confirmed the drain since the last fetch.
    drain_confirmed: bool,
}

impl<'a> LoopDrive<'a> {
    fn new(window: &'a mut Vec<FrontierEntry>, in_flight: &'a mut Vec<FrontierEntry>, max_concurrent: usize) -> Self {
        Self {
            window,
            in_flight,
            join_set: JoinSet::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            frontier_may_have_entries: true,
            drain_confirmed: false,
        }
    }
}

/// Snapshot the statistics a [`CrawlStrategy`] is asked to judge the crawl by.
fn crawl_stats(state: &CrawlState, start_time: Instant) -> CrawlStats {
    CrawlStats {
        pages_crawled: state.pages_processed(),
        pages_failed: state.pages_failed,
        urls_discovered: state.urls_discovered,
        urls_filtered: state.urls_filtered,
        elapsed: start_time.elapsed(),
    }
}

/// ~keep EnteredSpan is !Send, so dequeue spans must be entered and dropped before any `.await`.
fn emit_dequeue_span(entry: &FrontierEntry, window_len: usize, state: &CrawlState) {
    let _iter_span = tracing::info_span!(
        "crawl.loop.iteration",
        { CRAWL_DEPTH } = entry.depth as i64,
        { CRAWL_FRONTIER_SIZE } = (window_len + state.frontier_pending) as i64,
        { CRAWL_PAGES_COMPLETED } = state.pages_processed() as i64,
    )
    .entered();
}

/// Claim the seed's already-fetched response when `url` is the seed.
///
/// ~keep The seed was already fetched to resolve its redirect chain. Reusing that response
/// ~keep is what stops the crawl from issuing a second identical request for it; every other
/// ~keep URL still fetches normally.
fn take_preloaded_response(
    preloaded: &mut Option<(String, crate::tower::CrawlResponse, bool)>,
    url: &str,
) -> Option<(crate::tower::CrawlResponse, bool)> {
    match preloaded {
        Some((preloaded_url, _, _)) if preloaded_url == url => {
            preloaded.take().map(|(_, resp, browser_used)| (resp, browser_used))
        }
        _ => None,
    }
}

/// Fetch one URL, following any redirect it answers with, and run HTML extraction off the
/// runtime thread.
///
/// `permit` is held for the whole task so the semaphore bounds concurrent fetches.
///
/// ~keep A frontier entry is resolved through `follow_redirects` exactly like the seed is,
/// ~keep with a task-local `RedirectPolicy` built fresh here rather than shared across the
/// ~keep crawl: the policy's per-origin robots memoization is redundant with -- and no faster
/// ~keep than -- `CrawlEngine::robots_cache`, which every task already shares, so a fresh
/// ~keep policy per task needs no cross-task synchronization to stay correct.
async fn fetch_and_extract(
    engine: CrawlEngine,
    entry: FrontierEntry,
    preloaded_response: Option<(crate::tower::CrawlResponse, bool)>,
    permit: tokio::sync::OwnedSemaphorePermit,
    exclude_regexes: Arc<[Regex]>,
    include_regexes: Arc<[Regex]>,
) -> Result<FetchOutcome, (FrontierEntry, CrawlError)> {
    let _permit = permit;

    let (resp, browser_used, final_url, redirect_count) = match preloaded_response {
        // ~keep The seed's redirect chain was already resolved before the loop started; its
        // ~keep frontier entry URL is already the post-redirect final URL (see `seed_frontier`).
        Some((resp, browser_used)) => (resp, browser_used, entry.url.clone(), 0),
        None => {
            let client = crate::http::build_client(&engine.config).map_err(|e| (entry.clone(), e))?;
            let mut policy = RedirectPolicy::new(&engine, &client, exclude_regexes.as_ref(), include_regexes.as_ref());
            let max_redirects = engine.config.max_redirects;
            match follow_redirects(&engine, &entry.url, max_redirects, Some(&mut policy)).await {
                Ok(RedirectResolution::Fetched(outcome)) => (
                    outcome.final_response,
                    outcome.browser_used,
                    outcome.final_url,
                    outcome.redirect_count,
                ),
                // ~keep Refused only by a per-hop policy check (robots, exclude_paths, or a
                // ~keep dedup collision with a page already claimed elsewhere) -- the same
                // ~keep silent rejection `should_fetch_url` already applies to a frontier entry
                // ~keep the policy rejects before it is ever spawned.
                Ok(RedirectResolution::Refused { .. }) => return Ok(FetchOutcome::Skipped(entry)),
                Err(e) => return Err((entry.clone(), e)),
            }
        }
    };

    let status_code = resp.status;
    let content_type = resp.content_type;
    let headers = resp.headers;
    let body = resp.body;
    let body_bytes = resp.body_bytes;

    // ~keep The base URL for extraction is where the content actually came from. Using the
    // ~keep original `entry.url` here would resolve every relative link/asset on a redirected
    // ~keep page against the wrong origin.
    let url_for_extract = final_url.clone();
    let content_type_clone = content_type.clone();
    let robots_user_agent = crate::helpers::default_robots_user_agent(&engine.config).to_owned();
    let header_robots =
        crate::scrape::RobotsDirectives::from_header_values(headers.get("x-robots-tag"), &robots_user_agent);

    let page_ext = tokio::task::spawn_blocking(move || {
        blocking_extract_page(
            &url_for_extract,
            &content_type_clone,
            header_robots,
            &robots_user_agent,
            body,
            body_bytes,
        )
    })
    .await
    .map_err(|e| (entry.clone(), CrawlError::other(format!("extraction task failed: {e}"))))?;

    Ok(FetchOutcome::Fetched(Box::new(FetchResult {
        entry,
        status_code,
        content_type,
        body: page_ext.body,
        body_bytes: page_ext.body_bytes,
        headers,
        extraction: page_ext.extraction,
        robots: page_ext.robots,
        is_binary: page_ext.is_binary,
        is_pdf: page_ext.is_pdf,
        detected_charset: page_ext.detected_charset,
        final_url,
        redirect_count,
        browser_used,
    })))
}
