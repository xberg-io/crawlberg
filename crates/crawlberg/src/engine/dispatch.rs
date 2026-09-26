//! Escalation tiers: running one, choosing the next, and reporting what happened.

#![cfg(not(target_arch = "wasm32"))]

use super::CrawlEngine;
use crate::error::CrawlError;
use crate::tower::CrawlRequest;

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

impl CrawlEngine {
    /// Dispatch a single fetch attempt to the given tier.
    ///
    /// Returns `(CrawlResponse, browser_used)` or a `CrawlError`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) async fn run_tier(
        &self,
        tier: crate::types::Tier,
        url: &str,
        origin_host: Option<&str>,
    ) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        match tier {
            crate::types::Tier::Http => {
                let client = crate::http::build_client(&self.config)?;
                let mut service = self.build_service(&client);
                use tower::Service;
                let mut req = CrawlRequest::new(url).with_origin_host(origin_host.map(str::to_owned));
                req.tier = Some(Self::tier_name(tier));
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
                    },
                    false,
                ))
            }
            crate::types::Tier::Browser => {
                #[cfg(feature = "browser")]
                {
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
                headers: std::collections::HashMap::new(),
                landed: Some(crate::tower::Landing {
                    url: r.final_url,
                    redirects: page.redirects,
                }),
            },
            extras,
        )
    }

    /// Synthesise a minimal response with the given HTTP status (empty body).
    ///
    /// Used by `soft_http_errors` to surface 4xx responses as `ScrapeResult`
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
        }
    }

    /// Convert an [`crate::types::EscalationReason`] from a terminal success-path
    /// `Escalate` directive into the most specific available [`CrawlError`].
    ///
    /// Called when the policy signals `Escalate` on a 2xx response (soft-block /
    /// WAF interstitial) but no higher tier is available or the budget is exhausted.
    /// Returning an error prevents the challenge-page body from reaching callers.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn escalation_reason_to_error(reason: &crate::types::EscalationReason, url: &str) -> CrawlError {
        use crate::types::EscalationReason;
        match reason {
            EscalationReason::WafBlocked { vendor } => CrawlError::WafBlocked {
                vendor: vendor.clone(),
                message: format!("waf/blocked: {vendor} detected at {url}"),
            },
            EscalationReason::SoftBlock => CrawlError::forbidden(format!("soft_block: {url}")),
            EscalationReason::RenderNeeded => {
                CrawlError::unsupported(format!("js_render_needed but no browser tier available: {url}"))
            }
            EscalationReason::OriginUnreliable => {
                CrawlError::server_error(format!("origin_unreliable and no escalation target: {url}"))
            }
            EscalationReason::AntibotEscalate => CrawlError::WafBlocked {
                vendor: "antibot".to_string(),
                message: format!("antibot strategy forced browser escalation at {url}"),
            },
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
        // ~keep The field key stays `url` — it is public, semver-relevant surface — but the
        // value is redacted: a crawl of http://user:pass@host/ would otherwise put the
        // credential into every dispatch event.
        let url = crate::net::redact_url_credentials(url);
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
