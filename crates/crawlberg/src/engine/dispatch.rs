//! Escalation tiers: running one, choosing the next, and reporting what happened.

#![cfg(not(target_arch = "wasm32"))]

use super::CrawlEngine;
use crate::error::CrawlError;
use crate::tower::CrawlRequest;

/// The vendor an antibot strategy's refusal is reported and counted under.
const ANTIBOT_VENDOR: &str = "antibot";

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn escalation_reason_label(reason: &crate::types::EscalationReason) -> &'static str {
    use crate::types::EscalationReason;
    match reason {
        EscalationReason::WafBlocked { .. } => "waf_blocked",
        EscalationReason::SoftBlock => "soft_block",
        EscalationReason::RenderNeeded => "render_needed",
        EscalationReason::OriginUnreliable => "origin_unreliable",
        EscalationReason::AntibotEscalate => "antibot_escalate",
    }
}

/// Cheap content-density ratio: `text_bytes / html_bytes`.
///
/// Returns `0.0` for empty bodies. Uses a 5-line tag-stripping pass
/// (count chars outside `<...>`), NOT a full DOM parse — adequate for
/// detecting SPA shells (typical density 0.0–0.05) and soft-blocked
/// pages (typical density 0.0–0.1) vs. content pages (typical 0.3+).
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn content_density(body: &str) -> f32 {
    if body.is_empty() {
        return 0.0;
    }
    let total = body.len();
    let mut text = 0usize;
    let mut in_tag = false;
    for ch in body.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text += ch.len_utf8(),
            _ => {}
        }
    }
    text as f32 / total as f32
}

/// Whether robots.txt disallows the agent the Browser tier itself sends from fetching `url`.
///
/// ~keep The single re-judgment point for `run_tier`'s `Tier::Browser` arm (crawlberg#423): every
/// ~keep path that reaches the Browser tier through the dispatch loop's escalation passes through
/// ~keep here, judged against `default_robots_user_agent` -- the same agent
/// ~keep `crate::browser::browser_fetch` below actually sends, never a rotated pick.
/// ~keep `resolve_robots_outcome` is also what `admits` calls to judge the tier chosen at
/// ~keep admission, so the two never resolve the same origin two different ways.
#[cfg(feature = "browser")]
async fn robots_disallows_browser_agent(engine: &CrawlEngine, url: &str) -> Result<Option<String>, CrawlError> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Ok(Some(format!(
            "robots_unreachable: cannot parse {} to determine its origin",
            crate::net::redact_url_credentials(url)
        )));
    };
    let agent = crate::helpers::default_robots_user_agent(&engine.config);
    let client = crate::http::build_client(&engine.config)?;
    let outcome = super::redirect::resolve_robots_outcome(engine, &client, &parsed, url, agent).await;
    Ok(super::redirect::robots_block_reason(&outcome, &parsed))
}

impl CrawlEngine {
    /// Dispatch a single fetch attempt to the given tier.
    ///
    /// Returns `(CrawlResponse, browser_used)` or a `CrawlError`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) async fn run_tier(
        &self,
        tier: crate::types::Tier,
        url: &str,
        forced_user_agent: Option<&str>,
    ) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        match tier {
            crate::types::Tier::Http => {
                let client = crate::http::build_client(&self.config)?;
                let mut service = self.build_service(&client);
                use tower::Service;
                let mut req = CrawlRequest::new(url);
                req.tier = Some(Self::tier_name(tier));
                // ~keep Pins the agent `RedirectPolicy::admits` chose for the robots decision
                // ~keep onto the request, so the UA rotation layer (which only fills in a
                // ~keep `user-agent` header that is not already set) sends exactly that agent
                // ~keep instead of picking its own (crawlberg#423).
                if let Some(ua) = forced_user_agent {
                    req.headers.insert("user-agent".to_owned(), ua.to_owned());
                }
                let resp = service.call(req).await?;
                Ok((resp, false))
            }
            crate::types::Tier::Bypass => {
                let provider = self
                    .config
                    .dispatch
                    .as_ref()
                    .and_then(|d| d.bypass.as_ref())
                    .ok_or_else(|| {
                        CrawlError::invalid_config("escalation to Bypass tier but no bypass provider configured")
                    })?;
                let bypass_resp = provider.fetch(url).await?;
                Ok((
                    crate::tower::CrawlResponse {
                        status: bypass_resp.status,
                        content_type: bypass_resp.content_type,
                        body: bypass_resp.body,
                        body_bytes: bypass_resp.body_bytes,
                        headers: bypass_resp.headers,
                        landed: None,
                        // ~keep A custom bypass provider is a user plugin outside the rotation
                        // layer; it does not report which agent it sent, if any.
                        sent_user_agent: None,
                        soft_error: false,
                    },
                    false,
                ))
            }
            crate::types::Tier::Browser => {
                #[cfg(feature = "browser")]
                {
                    // ~keep A hop that reaches this arm by escalating mid-crawl (`BrowserMode::Auto`
                    // ~keep with `EscalationStrategy::BrowserOnly`/`BypassThenBrowser`) had its robots
                    // ~keep decision judged, in `admits`, against the Http tier's own agent -- chosen
                    // ~keep before the tier was known to end up here. The browser never sends that
                    // ~keep agent; it always sends `default_robots_user_agent`. Re-judge against that
                    // ~keep same agent right here, the one place every path into this tier passes
                    // ~keep through, so a disallow for the browser's own agent stops the fetch instead
                    // ~keep of a stale rotated-agent judgment letting it through (crawlberg#423).
                    // ~keep Gated on `forced_user_agent.is_some()`: that is only set once `admits` has
                    // ~keep run for this hop, i.e. a policy (`crawl()`) is in effect. `scrape()` passes
                    // ~keep no policy and, by design, reports robots status rather than enforcing it
                    // ~keep (see `resolve_robots_status` in `scrape.rs`); this check must not turn that
                    // ~keep report-only contract into enforcement for one tier alone.
                    if forced_user_agent.is_some()
                        && let Some(reason) = robots_disallows_browser_agent(self, url).await?
                    {
                        return Err(CrawlError::forbidden(reason));
                    }
                    let pool = self.config.browser_pool.as_deref();
                    #[cfg(feature = "browser-native")]
                    let page = crate::browser::browser_fetch(
                        url,
                        &self.config,
                        None,
                        pool,
                        false,
                        self.native_browser_executor.as_deref(),
                    )
                    .await?;
                    #[cfg(not(feature = "browser-native"))]
                    let page = crate::browser::browser_fetch(url, &self.config, None, pool, false).await?;
                    let (crawl_resp, _extras) = Self::browser_http_to_crawl(page);
                    Ok((crawl_resp, true))
                }
                #[cfg(not(feature = "browser"))]
                Err(CrawlError::unsupported("Browser tier requires the 'browser' feature"))
            }
        }
    }

    /// Convert a page from the browser path into the `CrawlResponse` shape expected by
    /// the extraction pipeline.
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser"))]
    pub(super) fn browser_http_to_crawl(
        page: crate::browser::BrowserPage,
    ) -> (crate::tower::CrawlResponse, Option<crate::http::BrowserExtras>) {
        let r = page.response;
        // ~keep `crate::tower::CrawlResponse` has no screenshot field (it is not owned by this
        // ~keep task and feeds every non-scrape() caller, including the multi-page crawl loop),
        // ~keep so a screenshot captured upstream in `page_fetch` cannot survive this conversion.
        // ~keep `CrawlEngine::scrape`'s dedicated short-circuit reads `HttpResponse.screenshot`
        // ~keep before calling this function specifically to avoid hitting this path; reaching
        // ~keep here with a screenshot still attached means some other caller (crawl loop,
        // ~keep dispatch escalation) requested one where it cannot be delivered.
        if r.screenshot.is_some() {
            tracing::warn!(
                "a page screenshot was captured but cannot be attached to this response path; discarding it. \
                 capture_screenshot is only delivered end-to-end by scrape() with BrowserBackend::Chromiumoxide \
                 and BrowserMode::Always or Stealth"
            );
        }
        let extras = r.browser_extras;
        (
            crate::tower::CrawlResponse {
                status: r.status,
                content_type: r.content_type,
                body: r.body,
                body_bytes: r.body_bytes,
                // ~keep Both browser backends collect the response headers; discarding them here
                // ~keep discarded them for every browser fetch on the crawl and escalation paths,
                // ~keep so `ETag`, `Cache-Control` and `X-Robots-Tag` reached no caller and no WAF
                // ~keep classifier however faithfully the backend had reported them (crawlberg#148).
                headers: r.headers,
                landed: Some(Box::new(crate::tower::Landing {
                    url: r.final_url,
                    redirects: page.redirects,
                    refused: page.refused,
                    extras: None,
                })),
                // ~keep The browser tier never reads `config.user_agents`; it always sends the
                // single configured agent, so callers fall back to the configured default.
                sent_user_agent: None,
                soft_error: false,
            },
            extras,
        )
    }

    /// Synthesise a minimal response with the given HTTP status (empty body).
    ///
    /// Used by `soft_http_errors` to surface error responses as `ScrapeResult`
    /// records rather than `CrawlError`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn synthesise_status(status: u16) -> crate::tower::CrawlResponse {
        crate::tower::CrawlResponse {
            status,
            content_type: String::new(),
            body: String::new(),
            body_bytes: Vec::new(),
            headers: std::collections::HashMap::new(),
            landed: None,
            sent_user_agent: None,
            soft_error: true,
        }
    }

    /// Convert an [`crate::types::EscalationReason`] from a terminal success-path
    /// `Escalate` directive into the most specific available [`CrawlError`].
    ///
    /// Called when a retry policy or an antibot strategy refuses a response the fetch
    /// accepted (a soft block or a WAF interstitial, served with any status) but no
    /// higher tier is available or the budget is exhausted. Returning an error prevents
    /// the challenge-page body from reaching callers. `status` is the status of the
    /// refused response; a WAF or soft block carries it as the error's source, as the
    /// fetch path's own WAF refusals do, so a `soft_http_errors` page can report it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn escalation_reason_to_error(
        reason: &crate::types::EscalationReason,
        url: &str,
        status: u16,
    ) -> CrawlError {
        use crate::types::EscalationReason;
        let source = crate::http::HttpStatus(status);
        match reason {
            EscalationReason::WafBlocked { vendor } => {
                CrawlError::waf_blocked_with_source(vendor.clone(), format!("{vendor} detected at {url}"), source)
            }
            EscalationReason::SoftBlock => CrawlError::forbidden_with_source(format!("soft_block: {url}"), source),
            EscalationReason::RenderNeeded => {
                CrawlError::unsupported(format!("js_render_needed but no browser tier available: {url}"))
            }
            EscalationReason::OriginUnreliable => {
                CrawlError::server_error(format!("origin_unreliable and no escalation target: {url}"))
            }
            EscalationReason::AntibotEscalate => CrawlError::waf_blocked_with_source(
                ANTIBOT_VENDOR,
                format!("antibot strategy forced browser escalation at {url}"),
                source,
            ),
        }
    }

    /// Count a successful response the engine refuses for `reason` in `crawl_waf_blocks_total`,
    /// when `reason` refuses it as a WAF block.
    ///
    /// ~keep The fetch path counts the responses it refuses itself; the engine only ever refuses
    /// a response the fetch path returned, so the two counts never cover the same response. A
    /// refusal is counted once it is final. A response the retry policy refuses while a higher
    /// tier is left is kept to hand back at the attempt cap, so it counts only if it is not
    /// handed back; every other refusal counts at once.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn record_waf_refusal(reason: &crate::types::EscalationReason) {
        if let Some(vendor) = Self::waf_refusal_vendor(reason) {
            crate::http::record_waf_block(vendor);
        }
    }

    /// The vendor a response the engine refuses for `reason` is counted under, or `None` when
    /// `reason` does not refuse it as a WAF block.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn waf_refusal_vendor(reason: &crate::types::EscalationReason) -> Option<&str> {
        use crate::types::EscalationReason;
        match reason {
            EscalationReason::WafBlocked { vendor } => Some(vendor),
            EscalationReason::AntibotEscalate => Some(ANTIBOT_VENDOR),
            EscalationReason::SoftBlock | EscalationReason::RenderNeeded | EscalationReason::OriginUnreliable => None,
        }
    }

    /// Determine the next tier given the current tier and active escalation strategy.
    ///
    /// Returns `None` when the current tier is terminal for the given strategy.
    ///
    /// Every `(Tier, EscalationStrategy)` combination is listed explicitly — no
    /// catch-all `_ => None`. This forces a compile error when new enum variants
    /// are added, matching the enforcement that `#[non_exhaustive]` provides for
    /// external consumers. Silently swallowing unknown combinations (the old
    /// catch-all) was the root cause of B2: `BypassFirst` was never handled.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn next_tier(
        current: crate::types::Tier,
        strategy: crate::types::EscalationStrategy,
    ) -> Option<crate::types::Tier> {
        use crate::types::{EscalationStrategy, Tier};
        match (current, strategy) {
            (Tier::Http, EscalationStrategy::BrowserOnly) => Some(Tier::Browser),
            (Tier::Http, EscalationStrategy::BypassOnly) => Some(Tier::Bypass),
            (Tier::Http, EscalationStrategy::BypassThenBrowser) => Some(Tier::Bypass),
            (Tier::Bypass, EscalationStrategy::BypassThenBrowser) => Some(Tier::Browser),
            // ~keep Keep explicit arms so new escalation strategies cause compile errors instead of silent truncation.
            (Tier::Bypass, EscalationStrategy::BypassFirst) => None,
            (Tier::Http, EscalationStrategy::BypassFirst) => None,
            (Tier::Browser, _) => None,
            (_, EscalationStrategy::None) => None,
            (Tier::Bypass, EscalationStrategy::BrowserOnly) => None,
            (Tier::Bypass, EscalationStrategy::BypassOnly) => None,
        }
    }

    /// Heuristic cost in internal "cents" for escalating to a tier.
    ///
    /// `Http` costs nothing (it's the baseline). `Bypass` and `Browser` cost 1 each
    /// so that `FixedBudget(n)` limits the total number of non-HTTP escalations per job.
    /// xberg-enterprise overrides this via a proper cost model at the cloud layer.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) const fn tier_cost_cents(tier: crate::types::Tier) -> u32 {
        match tier {
            crate::types::Tier::Http => 0,
            crate::types::Tier::Bypass | crate::types::Tier::Browser => 1,
        }
    }

    /// Stable lowercase name for a tier, used in span attributes and OTel labels.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) const fn tier_name(tier: crate::types::Tier) -> &'static str {
        match tier {
            crate::types::Tier::Http => "http",
            crate::types::Tier::Bypass => "bypass",
            crate::types::Tier::Browser => "browser",
        }
    }

    /// Stable lowercase string for an escalation reason.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn escalation_reason_str(reason: &crate::types::EscalationReason) -> &'static str {
        use crate::types::EscalationReason;
        match reason {
            EscalationReason::WafBlocked { .. } => "waf_blocked",
            EscalationReason::SoftBlock => "soft_block",
            EscalationReason::RenderNeeded => "render_needed",
            EscalationReason::OriginUnreliable => "origin_unreliable",
            EscalationReason::AntibotEscalate => "antibot_escalate",
        }
    }

    /// Emit structured dispatch telemetry via tracing.
    ///
    /// Fields: `dispatch.tier_chain`, `dispatch.escalation_reason`,
    /// `dispatch.attempt_count`, `dispatch.policy`, `dispatch.content_density`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn emit_dispatch_span(
        url: &str,
        tiers_attempted: &[&str],
        escalation_reason: Option<&str>,
        attempt_count: u32,
        policy: &str,
        content_density: f32,
    ) {
        let tier_chain = tiers_attempted.join(",");
        // ~keep The field key stays `url`: it is public, semver-relevant surface. The value
        // ~keep never holds userinfo, which the engine takes off every URL at admission.
        tracing::info!(
            target: "crawlberg::dispatch",
            url,
            "dispatch.tier_chain" = %tier_chain,
            "dispatch.escalation_reason" = escalation_reason.unwrap_or("none"),
            "dispatch.attempt_count" = attempt_count,
            "dispatch.policy" = policy,
            "dispatch.content_density" = content_density,
        );
    }
}

#[cfg(test)]
mod error_tests {
    use super::CrawlEngine;
    use crate::types::EscalationReason;

    #[test]
    fn a_waf_escalation_renders_one_prefix_and_keeps_its_status() {
        let error = CrawlEngine::escalation_reason_to_error(
            &EscalationReason::WafBlocked {
                vendor: "cloudflare".to_owned(),
            },
            "https://example.com/",
            503,
        );

        assert_eq!(
            error.to_string(),
            "forbidden: waf/blocked: cloudflare detected at https://example.com/"
        );
        assert_eq!(crate::http::error_status(&error), Some(503));
    }
}

#[cfg(all(test, feature = "browser"))]
mod tests {
    use super::CrawlEngine;

    #[test]
    fn a_browser_response_carries_its_headers_onto_the_crawl_path() {
        let response = crate::http::HttpResponse {
            status: 304,
            content_type: String::new(),
            body: String::new(),
            body_bytes: Vec::new(),
            headers: std::collections::HashMap::from([
                ("etag".to_owned(), vec!["\"v1\"".to_owned()]),
                ("x-robots-tag".to_owned(), vec!["noindex".to_owned()]),
            ]),
            browser_extras: None,
            final_url: "https://example.com/".to_owned(),
            screenshot: None,
        };

        let page = crate::browser::BrowserPage {
            response,
            redirects: 0,
            redirected: false,
            refused: vec!["http://127.0.0.1/secret".to_owned()],
        };
        let (crawl, _extras) = CrawlEngine::browser_http_to_crawl(page);

        let etag = crawl.headers.get("etag").expect("a browser fetch must report its ETag");
        assert_eq!(etag.as_slice(), ["\"v1\""]);
        let robots = crawl
            .headers
            .get("x-robots-tag")
            .expect("a browser fetch must report its X-Robots-Tag");
        assert_eq!(robots.as_slice(), ["noindex"]);
        assert_eq!(crawl.status, 304, "the status must survive the conversion too");
        assert_eq!(
            crawl.landed.map(|landed| landed.refused),
            Some(vec!["http://127.0.0.1/secret".to_owned()]),
            "the refused requests must survive the conversion"
        );
    }
}
