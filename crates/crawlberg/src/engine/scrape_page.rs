//! [`CrawlEngine::scrape`]: fetch one page and run the extraction pipeline over it.

use super::{CrawlEngine, SeedUrl};
use crate::error::CrawlError;
use crate::types::*;

impl CrawlEngine {
    /// Scrape a single URL, returning the extracted data.
    ///
    /// On native targets, routes the request through the Tower service stack
    /// (rate limiting, UA rotation) then runs the extraction pipeline.
    /// On wasm, performs a direct HTTP fetch without the Tower stack.
    ///
    /// Browser fallback behaviour (native + `browser` feature only):
    /// - `BrowserMode::Always`: skips HTTP entirely, goes straight to headless Chrome.
    /// - `BrowserMode::Auto` + WAF blocked: falls back to headless Chrome when the
    ///   Tower stack returns `CrawlError::WafBlocked`.
    /// - `BrowserMode::Auto` + JS detected: after extraction, if `js_render_hint` is
    ///   `true` and the browser has not been used yet, re-fetches with headless Chrome
    ///   and re-runs the extraction pipeline on the rendered HTML.
    pub async fn scrape(&self, url: &str) -> Result<ScrapeResult, CrawlError> {
        let (engine, seed) = self.admit(url)?;
        engine.scrape_seed(&seed).await
    }

    /// Scrape an admitted seed URL. See [`CrawlEngine::scrape`].
    #[tracing::instrument(name = "crawl.engine.scrape", skip_all, fields(url.full = %seed))]
    pub(crate) async fn scrape_seed(&self, seed: &SeedUrl) -> Result<ScrapeResult, CrawlError> {
        let url = seed.as_str();
        self.config.validate()?;

        #[cfg(not(target_arch = "wasm32"))]
        self.warn_if_capture_screenshot_is_a_no_op();

        // ~keep Short-circuit chromiumoxide BrowserMode::Always/Stealth when a screenshot was
        // ~keep requested: the generic `fetch_response`/`follow_redirects` path converts to
        // ~keep `CrawlResponse`, which drops the screenshot bytes captured on `HttpResponse`
        // ~keep (see `browser_http_to_crawl`).
        #[cfg(all(not(target_arch = "wasm32"), feature = "browser"))]
        if self.config.capture_screenshot
            && self.config.browser.backend == crate::types::BrowserBackend::Chromiumoxide
            && matches!(
                self.config.browser.mode,
                crate::types::BrowserMode::Always | crate::types::BrowserMode::Stealth
            )
        {
            return self.chromiumoxide_screenshot_scrape(url).await;
        }

        #[cfg(not(target_arch = "wasm32"))]
        let (final_url, response, browser_used_for_fetch) = {
            use super::redirect::{Hop, RedirectResolution, follow_redirects};

            let max_redirects = self.config.max_redirects;
            // ~keep A native render in BrowserMode::Always takes the same redirect chain as every
            // ~keep other scrape, so a meta refresh and a not-found seed end as in HTTP mode, and
            // ~keep it works on a build without the `browser` feature, which `fetch_response` needs.
            #[cfg(feature = "browser-native")]
            let hop = if self.config.browser.mode == crate::types::BrowserMode::Always
                && self.config.browser.backend == crate::types::BrowserBackend::Native
            {
                Hop::NativeRender
            } else {
                Hop::Fetch
            };
            #[cfg(not(feature = "browser-native"))]
            let hop = Hop::Fetch;
            let outcome = match follow_redirects(self, url, max_redirects, None, hop).await? {
                RedirectResolution::Fetched(outcome) => outcome,
                // ~keep Only a crawl policy refuses a hop, and a scrape passes none: it reports
                // ~keep robots.txt through `ScrapeResult::is_allowed` and fetches either way.
                // ~keep Reporting the refusal keeps this arm correct for a caller that does pass
                // ~keep one, where a panic or a discarded refusal would not be.
                RedirectResolution::Refused { refusal, .. } => return Err(refusal.into_error()),
            };

            let status = outcome.final_response.status;
            if matches!(status, 404 | 403) && outcome.final_response.body.is_empty() && self.config.soft_http_errors {
                return Ok(self.bodyless_status_result(status, outcome.final_url));
            }
            if outcome.final_response.status == 404
                && outcome.final_response.body.is_empty()
                && outcome.redirect_count > 0
            {
                return Ok(self.bodyless_status_result(404, outcome.final_url));
            }
            (outcome.final_url, outcome.final_response, outcome.browser_used)
        };

        #[cfg(target_arch = "wasm32")]
        let (final_url, response, browser_used_for_fetch) = self.wasm_fetch_for_scrape(url).await?;

        let mut result = crate::scrape::scrape_from_crawl_response(
            &final_url,
            &response,
            &self.config,
            self.document_filter.as_deref(),
        )
        .await?;
        result.browser_used = browser_used_for_fetch;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(extras) = response.landed.and_then(|landed| landed.extras) {
            result.browser = Some(crate::types::BrowserExtras {
                eval_result: extras.eval_result,
                network_events: extras.network_events,
                cookies: extras.cookies,
            });
        }

        // ~keep Without the browser feature, BrowserMode::Always still reports browser_used for binding parity.
        #[cfg(not(feature = "browser"))]
        if self.config.browser.mode == crate::types::BrowserMode::Always {
            result.browser_used = true;
        }

        Ok(result)
    }

    /// Warn when `capture_screenshot` is set on a configuration that cannot honour it.
    ///
    /// ~keep `capture_screenshot` is only ever honored by the chromiumoxide short-circuit in
    /// ~keep `scrape` (BrowserMode::Always/Stealth): every other path returns a
    /// ~keep `crate::tower::CrawlResponse`, which has no field to carry screenshot bytes
    /// ~keep through `redirect::follow_redirects`/`run_tier`. Warn proactively here instead of
    /// ~keep leaving the caller to discover the silent no-op from an empty
    /// ~keep `ScrapeResult::screenshot`.
    #[cfg(not(target_arch = "wasm32"))]
    fn warn_if_capture_screenshot_is_a_no_op(&self) {
        if !self.config.capture_screenshot {
            return;
        }
        let will_capture = cfg!(feature = "browser")
            && self.config.browser.backend == crate::types::BrowserBackend::Chromiumoxide
            && matches!(
                self.config.browser.mode,
                crate::types::BrowserMode::Always | crate::types::BrowserMode::Stealth
            );
        if !will_capture {
            tracing::warn!(
                backend = ?self.config.browser.backend,
                mode = ?self.config.browser.mode,
                "capture_screenshot has no effect for this configuration: scrape() only captures \
                 a screenshot with BrowserBackend::Chromiumoxide and BrowserMode::Always or Stealth"
            );
        }
    }

    /// Render `url` with the native backend, as one hop of a scrape's redirect chain.
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    pub(super) async fn native_render(&self, url: &str) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        let native_executor = self.native_browser_executor.as_deref().ok_or_else(|| {
            CrawlError::browser_error("native browser executor is not available for BrowserBackend::Native")
        })?;
        let (response, refused, redirects) =
            crate::native_browser::native_browser_fetch(url, &self.config, None, native_executor).await?;
        let response = crate::http::rendered_status_outcome(response, redirects > 0, &self.config)?;
        let crawl_resp = crate::tower::CrawlResponse {
            status: response.status,
            content_type: response.content_type,
            body: response.body,
            body_bytes: response.body_bytes,
            headers: response.headers,
            landed: Some(Box::new(crate::tower::Landing {
                url: response.final_url,
                redirects,
                refused,
                extras: response.browser_extras,
            })),
            // ~keep The native browser backend never reads `config.user_agents`.
            sent_user_agent: None,
        };
        Ok((crawl_resp, true))
    }

    /// Scrape through chromiumoxide, keeping the screenshot it captured.
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser"))]
    async fn chromiumoxide_screenshot_scrape(&self, url: &str) -> Result<ScrapeResult, CrawlError> {
        let pool = self.config.browser_pool.as_deref();
        #[cfg(feature = "browser-native")]
        let mut page = crate::browser::browser_fetch(
            url,
            &self.config,
            None,
            pool,
            true,
            self.native_browser_executor.as_deref(),
        )
        .await?;
        #[cfg(not(feature = "browser-native"))]
        let mut page = crate::browser::browser_fetch(url, &self.config, None, pool, true).await?;

        let screenshot = page.response.screenshot.take();
        let final_url = page.response.final_url.clone();
        let (crawl_resp, _extras) = Self::browser_http_to_crawl(page);
        let mut result = crate::scrape::scrape_from_crawl_response(
            &final_url,
            &crawl_resp,
            &self.config,
            self.document_filter.as_deref(),
        )
        .await?;
        result.browser_used = true;
        if let Some(bytes) = screenshot {
            result.screenshot_base64 = Some(crate::interact::encode_screenshot_base64(&bytes));
            result.screenshot = Some(bytes);
        }
        Ok(result)
    }

    /// The result reported for a status whose body is empty.
    ///
    /// ~keep Synthesized empty 4xx responses return minimal results instead of parsing an
    /// ~keep empty body as HTML.
    #[cfg(not(target_arch = "wasm32"))]
    fn bodyless_status_result(&self, status_code: u16, final_url: String) -> ScrapeResult {
        ScrapeResult {
            status_code,
            final_url,
            content_type: String::new(),
            html: String::new(),
            body_size: 0,
            metadata: PageMetadata::default(),
            links: Vec::new(),
            images: Vec::new(),
            feeds: Vec::new(),
            json_ld: Vec::new(),
            is_allowed: true,
            crawl_delay: None,
            noindex_detected: false,
            nofollow_detected: false,
            x_robots_tag: None,
            is_pdf: false,
            was_skipped: false,
            detected_charset: None,
            auth_header_sent: self.config.auth.is_some(),
            response_meta: None,
            assets: Vec::new(),
            js_render_hint: false,
            browser_used: false,
            markdown: None,
            extracted_data: None,
            extraction_meta: None,
            screenshot: None,
            screenshot_base64: None,
            downloaded_document: None,
            browser: None,
            ssrf_refused_urls: Vec::new(),
        }
    }

    /// Fetch `url` on wasm32, where the browser's own `fetch` already followed redirects.
    #[cfg(target_arch = "wasm32")]
    async fn wasm_fetch_for_scrape(
        &self,
        url: &str,
    ) -> Result<(String, crate::tower::CrawlResponse, bool), CrawlError> {
        let client = crate::http::build_client(&self.config)?;
        let resp = crate::http::fetch_with_retry(url, &self.config, &std::collections::HashMap::new(), &client).await?;
        // ~keep On wasm, browser fetch follows redirects; `resp.final_url` is the post-redirect URL.
        let post_redirect_url = resp.final_url.clone();
        let crawl_resp = crate::tower::CrawlResponse {
            status: resp.status,
            content_type: resp.content_type,
            body: resp.body,
            body_bytes: resp.body_bytes,
            headers: resp.headers,
            landed: None,
            // ~keep wasm has no UA rotation layer; every fetch sends `config.user_agent`.
            sent_user_agent: None,
        };
        Ok((post_redirect_url, crawl_resp, false))
    }
}
