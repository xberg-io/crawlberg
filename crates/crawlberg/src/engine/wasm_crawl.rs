//! The wasm32 crawl loop.
//!
//! ~keep The loop body also compiles under `cfg(test)` on native targets, so the tests at
//! ~keep the bottom of this file can exercise it: `wasm32-unknown-unknown` has no test
//! ~keep runner wired up in this repo, and without this the only check on this code is that
//! ~keep it compiles. Only the wasm32 entry points below are wasm-only.

#![cfg(any(target_arch = "wasm32", test))]

use super::link_scope::{LinkScopePolicy, link_in_scope};
use super::{CrawlEngine, DEFAULT_MAX_LINKS_PER_PAGE, take_selected};
use crate::error::CrawlError;
use crate::telemetry::attributes::URL_FULL;
use crate::traits::*;
use crate::types::*;

/// Wasm-specific sequential multi-page crawl implementations.
///
/// The native crawl loop uses `tokio::spawn`, `JoinSet`, and `Semaphore` which do not
/// compile to `wasm32-unknown-unknown`. These implementations drive the same BFS/DFS/
/// strategy logic sequentially using `.await` only — no concurrency primitives.
impl CrawlEngine {
    /// Convert a `ScrapeResult` into a `CrawlPageResult` at the given depth.
    fn scrape_to_crawl_page(scrape: ScrapeResult, url: &str, depth: usize, base_host: &str) -> CrawlPageResult {
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_owned()))
            .unwrap_or_default();
        let stayed_on_domain = domain == base_host;
        CrawlPageResult {
            url: url.to_owned(),
            normalized_url: crate::normalize::normalize_url(url),
            status_code: scrape.status_code,
            content_type: scrape.content_type,
            html: scrape.html,
            body_size: scrape.body_size,
            metadata: scrape.metadata,
            links: scrape.links,
            images: scrape.images,
            feeds: scrape.feeds,
            json_ld: scrape.json_ld,
            depth,
            stayed_on_domain,
            was_skipped: scrape.was_skipped,
            is_pdf: scrape.is_pdf,
            detected_charset: scrape.detected_charset,
            markdown: scrape.markdown,
            extracted_data: scrape.extracted_data,
            extraction_meta: scrape.extraction_meta,
            downloaded_document: scrape.downloaded_document,
            browser_used: scrape.browser_used,
            // ~keep The browser's own `fetch` already followed redirects (see `scrape`'s
            // ~keep wasm32 path), so `scrape.final_url` is this page's post-redirect URL and
            // ~keep there is no separate per-hop count to report here.
            final_url: scrape.final_url,
            redirect_count: 0,
        }
    }

    /// Crawl a website starting from `url`.
    ///
    /// Implements a sequential BFS/strategy-driven crawl loop. Follows links discovered
    /// during scraping and applies `max_depth`, `max_pages`, host scope,
    /// `allow_subdomains`, `include_paths`, `exclude_paths`, and the configured
    /// `CrawlStrategy`. No concurrency primitives are used — each page is awaited
    /// sequentially, which is correct for the wasm single-threaded executor.
    pub(super) async fn crawl_sequential(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        self.config.validate()?;

        let plan = SequentialPlan::new(url, &self.config)?;
        let robots = self.load_sequential_robots(url).await?;
        if let Some(reason) = robots.disallow_all_reason() {
            return Ok(self.robots_blocked_result(url, reason).await);
        }

        self.seed_sequential_frontier(url).await?;

        let mut state = SequentialState::new(url);
        self.drive_sequential_loop(&plan, &robots, &mut state).await?;
        self.finish_sequential_crawl(state, plan.max_pages).await
    }

    /// Read the seed's robots.txt.
    ///
    /// ~keep robots.txt is read before the seed is pushed, so nothing is fetched before the
    /// site's policy is known. This loop previously never read `respect_robots_txt` at all:
    /// on wasm the setting was silently ignored and every URL was fetched.
    async fn load_sequential_robots(&self, url: &str) -> Result<crate::helpers::RobotsOutcome, CrawlError> {
        if !self.config.respect_robots_txt {
            return Ok(crate::helpers::RobotsOutcome::AllowAll);
        }
        let client = crate::http::build_client(&self.config)?;
        Ok(crate::helpers::fetch_robots_outcome(
            url,
            &self.config,
            &client,
            crate::helpers::default_robots_user_agent(&self.config),
        )
        .await)
    }

    /// The result for a site whose robots.txt forbids the crawl outright.
    async fn robots_blocked_result(&self, url: &str, reason: &str) -> CrawlResult {
        let error = format!("robots_unreachable: {reason}");
        self.event_emitter
            .on_error(&crate::traits::ErrorEvent {
                url: url.to_owned(),
                error: error.clone(),
            })
            .await;
        let _ = self
            .store
            .on_complete(&CrawlStats {
                pages_crawled: 0,
                pages_failed: 0,
                urls_discovered: 0,
                urls_filtered: 0,
                elapsed: std::time::Duration::ZERO,
            })
            .await;
        self.event_emitter
            .on_complete(&crate::traits::CompleteEvent { pages_crawled: 0 })
            .await;
        // ~keep `was_skipped` + `error` rather than a new field: `CrawlResult` is generated
        // into every language binding and is not `#[non_exhaustive]`.
        CrawlResult::new(CrawlOutcome {
            pages: Vec::new(),
            final_url: url.to_owned(),
            redirect_count: 0,
            was_skipped: true,
            error: Some(error),
            cookies: Vec::new(),
            stayed_on_domain: true,
        })
    }

    /// Put the seed on the frontier as the depth-0 entry, marking it seen first.
    async fn seed_sequential_frontier(&self, url: &str) -> Result<(), CrawlError> {
        let seed_dedup = crate::normalize::normalize_url_for_dedup(url);
        self.frontier.mark_seen(&seed_dedup).await?;
        self.frontier
            .push(FrontierEntry {
                url: url.to_owned(),
                depth: 0,
                doc_depth: 0,
                priority: 1.0,
            })
            .await
    }

    /// Fetch one page at a time until a limit, the strategy, or an empty frontier stops it.
    async fn drive_sequential_loop(
        &self,
        plan: &SequentialPlan,
        robots: &crate::helpers::RobotsOutcome,
        state: &mut SequentialState,
    ) -> Result<(), CrawlError> {
        loop {
            if state.window.is_empty() {
                state.window = self.frontier.pop_batch(1).await?;
                if state.window.is_empty() {
                    break;
                }
            }

            if !self.strategy.should_continue(&state.stats()) {
                break;
            }
            if state.pages.len() >= plan.max_pages {
                break;
            }

            let Some((_index, entry)) = take_selected(self.strategy.as_ref(), &mut state.window) else {
                break;
            };

            if !passes_url_filters(&entry, plan, robots, state) {
                continue;
            }

            if !self.budget_admits_sequential_page().await {
                break;
            }

            let Some(scrape) = self.scrape_for_sequential_crawl(&entry, state).await else {
                continue;
            };

            state.note_page_outcome(&entry, &scrape);

            if self.should_discover_sequentially(&entry, &scrape, plan) {
                self.discover_sequential_links(&scrape, &entry, plan, state).await?;
            }

            let page_url = scrape.final_url.clone();
            let page = Self::scrape_to_crawl_page(scrape, &page_url, entry.depth, &plan.base_host);

            let Some(page) = self.content_filter.filter(page).await? else {
                state.urls_filtered += 1;
                continue;
            };

            self.strategy.on_page_processed(&page);
            let _ = self.store.store_crawl_page(&page.url, &page).await;
            self.event_emitter
                .on_page(&crate::traits::PageEvent {
                    url: page.url.clone(),
                    status_code: page.status_code,
                    depth: page.depth,
                })
                .await;

            state.pages.push(page);
        }

        Ok(())
    }

    /// Whether the page budget admits another fetch.
    async fn budget_admits_sequential_page(&self) -> bool {
        match self.page_budget.check().await {
            Ok(()) => true,
            Err(crate::budget::BudgetError::Exhausted) => {
                tracing::info!(target: "crawlberg.budget", "page budget exhausted");
                false
            }
            Err(crate::budget::BudgetError::Backend(msg)) => {
                // ~keep Degraded, not data loss: the crawl still returns whatever pages
                // ~keep were already fetched, so this belongs at WARN, not ERROR.
                tracing::warn!(target: "crawlberg.budget", error = %msg, "budget backend error; treating as exhausted");
                false
            }
        }
    }

    /// Scrape one entry, reporting a failure and returning `None` rather than aborting.
    async fn scrape_for_sequential_crawl(
        &self,
        entry: &FrontierEntry,
        state: &mut SequentialState,
    ) -> Option<ScrapeResult> {
        match self.scrape(&entry.url).await {
            Ok(scrape) => Some(scrape),
            Err(e) => {
                state.pages_failed += 1;
                let error_msg = e.to_string();
                self.event_emitter
                    .on_error(&crate::traits::ErrorEvent {
                        url: entry.url.clone(),
                        error: error_msg.clone(),
                    })
                    .await;
                let _ = self.store.store_error(&entry.url, &e).await;
                if entry.depth == 0 {
                    state.crawl_error = Some(error_msg);
                }
                None
            }
        }
    }

    /// Whether this page's links are followed at all.
    fn should_discover_sequentially(
        &self,
        entry: &FrontierEntry,
        scrape: &ScrapeResult,
        plan: &SequentialPlan,
    ) -> bool {
        let in_doc_context = entry.doc_depth > 0;
        let page_is_skipped = scrape.was_skipped || scrape.is_pdf;
        entry.depth < plan.max_depth && !page_is_skipped && (!in_doc_context || self.config.follow_document_urls)
    }

    /// Enqueue the eligible links of one page, up to the per-page cap.
    async fn discover_sequential_links(
        &self,
        scrape: &ScrapeResult,
        entry: &FrontierEntry,
        plan: &SequentialPlan,
        state: &mut SequentialState,
    ) -> Result<(), CrawlError> {
        // ~keep A single page can carry unbounded link fan-out (e.g. a sitemap-like page
        // ~keep with a million anchors); cap per-page discovery so one page cannot blow
        // ~keep up frontier memory in a single iteration.
        // ~keep Mirrors the native crawl_loop: `max_links_per_page` is user-settable and
        // ~keep the constant is only the fallback. Hardcoding it here silently ignored the
        // ~keep caller's setting on wasm, which native honours.
        let link_cap = self.config.max_links_per_page.unwrap_or(DEFAULT_MAX_LINKS_PER_PAGE);
        // ~keep Counts links actually *enqueued*, exactly as the native loop counts
        // accepted candidates. Taking the first `link_cap` raw anchors instead would
        // let a run of external or already-seen links exhaust the budget and hide
        // eligible internal links sitting behind them — a page whose first 10,000
        // anchors are outbound would discover nothing at all on wasm and everything
        // on native.
        let mut enqueued_from_page = 0usize;

        for link in &scrape.links {
            if enqueued_from_page >= link_cap {
                tracing::warn!(
                    target: "crawlberg.frontier",
                    url = %entry.url,
                    link_count = scrape.links.len(),
                    cap = link_cap,
                    "page link fan-out exceeds cap, truncating discovered links"
                );
                break;
            }

            let link_url = crate::normalize::strip_fragment(&link.url);
            let scope_policy = LinkScopePolicy {
                follow_document_urls: self.config.follow_document_urls,
                document_url_depth: self.config.document_url_depth,
                allow_subdomains: self.config.allow_subdomains,
                base_host: &plan.base_host,
                base_host_suffix: &plan.base_host_suffix,
            };
            if !link_in_scope(link, &link_url, entry.doc_depth, &scope_policy) {
                continue;
            }

            let is_doc_link = link.link_type == LinkType::Document;
            if self
                .enqueue_discovered_link(&link_url, is_doc_link, entry, state)
                .await?
            {
                enqueued_from_page += 1;
            }
        }

        Ok(())
    }

    /// Push one discovered link, reporting whether it was new to the frontier.
    async fn enqueue_discovered_link(
        &self,
        link_url: &str,
        is_doc_link: bool,
        entry: &FrontierEntry,
        state: &mut SequentialState,
    ) -> Result<bool, CrawlError> {
        // ~keep Dedup goes through the frontier, not a loop-local set: the frontier
        // owns the queue, so a persistent implementation that survives a restart
        // would otherwise re-enqueue every URL it had already crawled.
        let dedup_key = crate::normalize::normalize_url_for_dedup(link_url);
        if self.frontier.is_seen(&dedup_key).await? {
            return Ok(false);
        }

        self.frontier.mark_seen(&dedup_key).await?;
        let child_depth = entry.depth + 1;
        let child_doc_depth: u32 = if is_doc_link { entry.doc_depth + 1 } else { 0 };
        let priority = self.strategy.score_url(link_url, child_depth);
        self.frontier
            .push(FrontierEntry {
                url: link_url.to_owned(),
                depth: child_depth,
                doc_depth: child_doc_depth,
                priority,
            })
            .await?;
        state.urls_discovered += 1;
        self.event_emitter.on_discovered(link_url, child_depth).await;
        Ok(true)
    }

    /// Emit the terminal events and build the result.
    async fn finish_sequential_crawl(
        &self,
        mut state: SequentialState,
        max_pages: usize,
    ) -> Result<CrawlResult, CrawlError> {
        // ~keep Return the unfetched entry the loop was holding when a limit stopped it, so a
        // persistent or distributed frontier does not lose queued work.
        for entry in std::mem::take(&mut state.window) {
            self.frontier.push(entry).await?;
        }

        let _ = self.store.on_complete(&state.stats()).await;
        self.event_emitter
            .on_complete(&crate::traits::CompleteEvent {
                pages_crawled: state.pages.len(),
            })
            .await;

        if state.pages.len() > max_pages {
            state.pages.truncate(max_pages);
        }

        let stayed_on_domain = state.pages.iter().all(|p| p.stayed_on_domain);
        Ok(CrawlResult::new(CrawlOutcome {
            pages: state.pages,
            final_url: state.final_url,
            redirect_count: state.redirect_count,
            was_skipped: state.was_skipped,
            error: state.crawl_error,
            cookies: Vec::new(),
            stayed_on_domain,
        }))
    }
}

#[cfg(target_arch = "wasm32")]
impl CrawlEngine {
    /// Crawl a website starting from `url`.
    ///
    /// See [`CrawlEngine::crawl_sequential`] for the loop this delegates to.
    #[tracing::instrument(name = "crawl.engine.crawl", skip(self), fields(url.full = tracing::field::Empty))]
    pub async fn crawl(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        self.crawl_sequential(url).await
    }

    /// Scrape multiple URLs sequentially (no concurrency on wasm).
    #[tracing::instrument(name = "crawl.engine.batch_scrape", skip(self, urls), fields(url_count = urls.len()))]
    pub async fn batch_scrape(&self, urls: &[&str]) -> Vec<(String, Result<ScrapeResult, CrawlError>)> {
        let mut results = Vec::with_capacity(urls.len());
        for url in urls {
            let result = self.scrape(url).await;
            results.push((url.to_string(), result));
        }
        results
    }

    /// Crawl multiple seed URLs sequentially (no concurrency on wasm).
    #[tracing::instrument(name = "crawl.engine.batch", skip(self, urls), fields(crawl.seed_count = urls.len()))]
    pub async fn batch_crawl(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        let mut results = Vec::with_capacity(urls.len());
        for url in urls {
            let result = self.crawl(url).await;
            results.push((url.to_string(), result));
        }
        results
    }
}

/// The crawl-wide inputs the sequential loop reads but never changes.
struct SequentialPlan {
    base_host: String,
    base_host_suffix: String,
    max_depth: usize,
    max_pages: usize,
    exclude_regexes: Vec<regex::Regex>,
    include_regexes: Vec<regex::Regex>,
}

impl SequentialPlan {
    fn new(url: &str, config: &CrawlConfig) -> Result<Self, CrawlError> {
        let parsed_seed = url::Url::parse(url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
        let base_host = parsed_seed.host_str().unwrap_or("").to_owned();
        Ok(Self {
            base_host_suffix: format!(".{base_host}"),
            base_host,
            max_depth: config.max_depth.unwrap_or(usize::MAX),
            max_pages: config.max_pages.unwrap_or(usize::MAX),
            exclude_regexes: crate::helpers::compile_regexes(&config.exclude_paths)?,
            include_regexes: crate::helpers::compile_regexes(&config.include_paths)?,
        })
    }
}

/// The state one sequential crawl accumulates.
struct SequentialState {
    /// ~keep One page is fetched at a time here, so the window holds a single entry and is
    /// refilled when empty: the frontier's own ordering is therefore the visit order
    /// exactly, which is what makes InMemoryFrontier breadth-first and LifoFrontier
    /// depth-first on this path too.
    window: Vec<FrontierEntry>,
    pages: Vec<CrawlPageResult>,
    redirect_count: usize,
    was_skipped: bool,
    pages_failed: usize,
    urls_discovered: usize,
    urls_filtered: usize,
    crawl_error: Option<String>,
    final_url: String,
}

impl SequentialState {
    fn new(seed_url: &str) -> Self {
        Self {
            window: Vec::with_capacity(1),
            pages: Vec::new(),
            redirect_count: 0,
            was_skipped: false,
            pages_failed: 0,
            urls_discovered: 0,
            urls_filtered: 0,
            crawl_error: None,
            final_url: seed_url.to_owned(),
        }
    }

    fn stats(&self) -> CrawlStats {
        CrawlStats {
            pages_crawled: self.pages.len(),
            pages_failed: self.pages_failed,
            urls_discovered: self.urls_discovered,
            urls_filtered: self.urls_filtered,
            elapsed: std::time::Duration::ZERO,
        }
    }

    /// Record what a fetched page says about the crawl as a whole.
    fn note_page_outcome(&mut self, entry: &FrontierEntry, scrape: &ScrapeResult) {
        if entry.depth == 0 {
            self.final_url = scrape.final_url.clone();
            if scrape.final_url != entry.url {
                self.redirect_count += 1;
            }
        }
        if scrape.was_skipped || scrape.is_pdf {
            self.was_skipped = true;
        }
    }
}

/// Whether `entry` survives the path filters and robots.txt, counting it if it does not.
///
/// ~keep An entry whose URL does not parse is admitted: the original loop applied these
/// ~keep rules only inside `if let Ok(parsed)`, letting the fetch report the failure.
fn passes_url_filters(
    entry: &FrontierEntry,
    plan: &SequentialPlan,
    robots: &crate::helpers::RobotsOutcome,
    state: &mut SequentialState,
) -> bool {
    let Ok(parsed) = url::Url::parse(&entry.url) else {
        return true;
    };
    let path = parsed.path();

    if !plan.exclude_regexes.is_empty() && plan.exclude_regexes.iter().any(|re| re.is_match(path)) {
        state.urls_filtered += 1;
        return false;
    }
    if !plan.include_regexes.is_empty() && entry.depth > 0 && !plan.include_regexes.iter().any(|re| re.is_match(path)) {
        state.urls_filtered += 1;
        return false;
    }
    if !robots.allows(path) {
        state.urls_filtered += 1;
        return false;
    }

    true
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::types::CrawlConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_html(mock: &MockServer, at: &str, body: &str) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body.to_owned())
                    .append_header("content-type", "text/html"),
            )
            .mount(mock)
            .await;
    }

    /// Root links to `/a`, `/b`, `/c` in that order; each child links to one grandchild.
    async fn branching_site() -> MockServer {
        let mock = MockServer::start().await;
        mount_html(
            &mock,
            "/",
            r#"<html><body><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a></body></html>"#,
        )
        .await;
        for child in ["a", "b", "c"] {
            mount_html(
                &mock,
                &format!("/{child}"),
                &format!(r#"<html><body><a href="/{child}1">{child}1</a></body></html>"#),
            )
            .await;
            mount_html(
                &mock,
                &format!("/{child}1"),
                &format!("<html><body>leaf {child}1</body></html>"),
            )
            .await;
        }
        mock
    }

    fn engine_with(config: CrawlConfig) -> CrawlEngine {
        CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build")
    }

    fn permissive(config: CrawlConfig) -> CrawlConfig {
        CrawlConfig {
            ssrf: crate::net::SsrfPolicy {
                deny_private: false,
                ..crate::net::SsrfPolicy::default()
            },
            ..config
        }
    }

    fn visited(result: &CrawlResult, base: &str) -> Vec<String> {
        result
            .pages
            .iter()
            .map(|page| match page.url.strip_prefix(base).unwrap_or(&page.url) {
                "" => "/".to_owned(),
                rest => rest.to_owned(),
            })
            .collect()
    }

    /// The sequential loop visits one depth level before the next, in document order.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_visits_breadth_first() {
        let mock = branching_site().await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(2),
            max_pages: Some(4),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
            "the whole depth-1 level must be visited before any depth-2 page"
        );
    }

    /// `max_depth` bounds how far links are followed, not just how many pages are kept.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_stops_following_links_at_max_depth() {
        let mock = branching_site().await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
            "no grandchild may be reached at max_depth = 1"
        );
    }

    /// `max_links_per_page` caps how many links one page may enqueue.
    ///
    /// ~keep The cap counts links actually *enqueued*, not raw anchors examined; a page
    /// ~keep whose first anchors are external must still discover the eligible ones behind
    /// ~keep them.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_caps_links_enqueued_per_page() {
        let mock = branching_site().await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            max_links_per_page: Some(2),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned()],
            "only the first two eligible links of the root may be enqueued"
        );
    }

    /// An excluded path is filtered out before it is fetched.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_drops_excluded_paths() {
        let mock = branching_site().await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            exclude_paths: vec!["^/b$".to_owned()],
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned(), "/a".to_owned(), "/c".to_owned()],
            "`/b` matches exclude_paths and must never be fetched"
        );
    }

    /// Regression coverage for crawlberg#60 on the sequential (wasm) loop: a subdomain link
    /// must be followed when `allow_subdomains` is true.
    ///
    /// ~keep Uses `*.localhost`, not a fabricated hostname: this positive case needs a real,
    /// reachable second host to prove the link is actually followed rather than merely not
    /// rejected. RFC 6761 §6.3 requires every conformant resolver to resolve `*.localhost` to
    /// the loopback address without any network traffic, unlike a public-DNS trick such as
    /// nip.io. The negative cases below use a fabricated `*.example.invalid` host instead,
    /// since a rejected link never reaches DNS resolution (see their own doc comments).
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_follows_subdomain_link_when_allow_subdomains_is_true() {
        let mock = MockServer::start().await;
        let port = mock.address().port();
        mount_html(
            &mock,
            "/",
            &format!(r#"<html><body><a href="http://sub.localhost:{port}/a">A</a></body></html>"#),
        )
        .await;
        mount_html(&mock, "/a", "<html><body>a</body></html>").await;
        let base = format!("http://localhost:{port}");
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: true,
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            result.pages.len(),
            2,
            "a subdomain link must be followed when allow_subdomains is true, got pages: {:?}",
            result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
        );
    }

    /// The same subdomain link must NOT be followed when `allow_subdomains` is false.
    ///
    /// ~keep A rejected link is dropped before SSRF validation ever resolves DNS, so a
    /// fabricated, unregistered hostname (RFC 2606's `.invalid`) is safe here — no network
    /// access happens for it either way.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_rejects_subdomain_link_when_allow_subdomains_is_false() {
        let mock = MockServer::start().await;
        mount_html(
            &mock,
            "/",
            r#"<html><body><a href="https://sub.example.invalid/a">A</a></body></html>"#,
        )
        .await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: false,
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            result.pages.len(),
            1,
            "a subdomain link must not be followed when allow_subdomains is false, got pages: {:?}",
            result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
        );
    }

    /// An unrelated host is never enqueued by a default-configured crawl.
    ///
    /// ~keep This pins the additive contract of the crawlberg#60 fix. The host is deliberately
    /// unresolvable (`.invalid`, RFC 2606): scope rejects it before SSRF validation would
    /// resolve anything, so the test needs no DNS. `stay_on_domain` is not an input -- see
    /// `link_scope::host_in_scope` and crawlberg#72.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_rejects_an_unrelated_host_by_default() {
        let mock = MockServer::start().await;
        let port = mock.address().port();
        mount_html(
            &mock,
            "/",
            &format!(r#"<html><body><a href="http://unrelated.invalid:{port}/a">A</a></body></html>"#),
        )
        .await;
        let base = format!("http://localhost:{port}");
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            result.pages.len(),
            1,
            "an unrelated host must not be followed, got pages: {:?}",
            result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
        );
    }

    /// An off-host link is never enqueued: the seed host and, with `allow_subdomains`, its
    /// subdomains are the only hosts a crawl follows. ~keep `stay_on_domain` is NOT what
    /// enforces this and never has -- see `link_scope::host_in_scope` and crawlberg#72.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_stays_on_the_seed_host() {
        let mock = MockServer::start().await;
        mount_html(
            &mock,
            "/",
            r#"<html><body><a href="https://elsewhere.example.com/x">out</a><a href="/a">A</a></body></html>"#,
        )
        .await;
        mount_html(&mock, "/a", "<html><body>a</body></html>").await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned(), "/a".to_owned()],
            "an off-host link must not be enqueued"
        );
    }

    /// A seed that fails is reported through `CrawlResult::error`; a child that fails is not.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_reports_a_seed_failure_but_not_a_child_failure() {
        let seed_down = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&seed_down)
            .await;
        let engine = engine_with(permissive(CrawlConfig::default()));
        let result = engine
            .crawl_sequential(&seed_down.uri())
            .await
            .expect("a failing seed is still a completed crawl");
        assert!(result.pages.is_empty(), "a failing seed produces no pages");
        assert!(
            result.error.is_some(),
            "a depth-0 failure must surface as CrawlResult::error"
        );

        let child_down = MockServer::start().await;
        mount_html(&child_down, "/", r#"<html><body><a href="/a">A</a></body></html>"#).await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&child_down)
            .await;
        let base = child_down.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(1),
            ..CrawlConfig::default()
        }));
        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");
        assert_eq!(
            visited(&result, &base),
            vec!["/".to_owned()],
            "the failing child contributes no page"
        );
        assert!(
            result.error.is_none(),
            "a failure below depth 0 must not become the crawl's error"
        );
    }

    /// A seed that redirects is counted once and reported under its post-redirect URL.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn sequential_crawl_counts_a_seed_redirect() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/landing"))
            .mount(&mock)
            .await;
        mount_html(&mock, "/landing", "<html><body>landed</body></html>").await;
        let base = mock.uri();
        let engine = engine_with(permissive(CrawlConfig {
            max_depth: Some(0),
            ..CrawlConfig::default()
        }));

        let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

        assert_eq!(result.redirect_count, 1, "the seed hop must be counted once");
        assert_eq!(
            result.final_url,
            format!("{base}/landing"),
            "final_url must be the post-redirect URL"
        );
    }
}
