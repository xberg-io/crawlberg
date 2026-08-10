//! CrawlEngine composes trait implementations into a crawl pipeline.

#[cfg(not(target_arch = "wasm32"))]
mod batch;
mod builder;
#[cfg(not(target_arch = "wasm32"))]
mod crawl_loop;

#[cfg(not(target_arch = "wasm32"))]
use opentelemetry::KeyValue;
use std::sync::Arc;

use crate::error::CrawlError;
use crate::telemetry::attributes::URL_FULL;

#[cfg(not(target_arch = "wasm32"))]
fn escalation_reason_label(reason: &crate::types::EscalationReason) -> &'static str {
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
pub(crate) fn content_density(body: &str) -> f32 {
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
#[cfg(not(target_arch = "wasm32"))]
use crate::sink::EventSink;
#[cfg(not(target_arch = "wasm32"))]
use crate::tower::CrawlRequest;
use crate::traits::*;
use crate::types::*;

pub use builder::CrawlEngineBuilder;

/// The main crawl engine, composed of pluggable trait implementations.
#[derive(Clone)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct CrawlEngine {
    pub(crate) config: CrawlConfig,
    pub(crate) frontier: Arc<dyn Frontier>,
    pub(crate) rate_limiter: Arc<dyn RateLimiter>,
    pub(crate) store: Arc<dyn CrawlStore>,
    pub(crate) event_emitter: Arc<dyn EventEmitter>,
    pub(crate) strategy: Arc<dyn CrawlStrategy>,
    pub(crate) content_filter: Arc<dyn ContentFilter>,
    pub(crate) cache: Arc<dyn CrawlCache>,
    /// Optional event sink for streaming crawl events to external consumers
    /// (e.g., NATS, dashboards, analytics).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) event_sink: Option<Arc<dyn EventSink>>,
    /// Optional page budget hook for enforcing per-crawl page allowances.
    #[allow(dead_code)]
    pub(crate) page_budget: Arc<dyn crate::budget::PageBudget>,
    /// Shared UA rotation layer — preserves rotation counter across service builds.
    #[cfg(not(target_arch = "wasm32"))]
    ua_rotation: crate::tower::UaRotationLayer,
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    pub(crate) native_browser_executor: Option<Arc<crawlberg_browser::adapter::NativeBrowserExecutor>>,
}

impl CrawlEngine {
    /// Create a new [`CrawlEngineBuilder`].
    pub fn builder() -> CrawlEngineBuilder {
        CrawlEngineBuilder::new()
    }

    /// Build the Tower service stack for HTTP fetching.
    ///
    /// Layers (outermost to innermost):
    /// 1. Per-domain rate limiting
    /// 2. HTTP response caching
    /// 3. User-agent rotation
    /// 4. Base HTTP fetch
    #[cfg(not(target_arch = "wasm32"))]
    fn build_service(
        &self,
        client: &reqwest::Client,
    ) -> tower::util::BoxCloneService<CrawlRequest, crate::tower::CrawlResponse, CrawlError> {
        use tower::ServiceBuilder;

        let service = ServiceBuilder::new()
            .layer(crate::tower::PerDomainRateLimitLayer::new(self.rate_limiter.clone()))
            .layer(crate::tower::CrawlCacheLayer::new(self.cache.clone()))
            .layer(self.ua_rotation.clone())
            .service(crate::tower::HttpFetchService::new(client.clone(), self.config.clone()));

        let service = tower::ServiceBuilder::new()
            .layer(crate::tower::CrawlTracingLayer::new())
            .service(service);

        tower::util::BoxCloneService::new(service)
    }

    /// Fetch a URL through the appropriate path (Tower stack or browser) and
    /// return the `CrawlResponse` together with a flag indicating whether the
    /// browser was used.
    ///
    /// This is intentionally `#[cfg(not(target_arch = "wasm32"))]`-only: wasm
    /// has its own simpler inline path inside `scrape`.
    #[cfg(not(target_arch = "wasm32"))]
    async fn fetch_response(&self, url: &str) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        #[cfg(feature = "browser")]
        if matches!(
            self.config.browser.mode,
            crate::types::BrowserMode::Always | crate::types::BrowserMode::Stealth
        ) {
            let pool = self.config.browser_pool.as_deref();
            #[cfg(feature = "browser-native")]
            let http_resp =
                crate::browser::browser_fetch(url, &self.config, None, pool, self.native_browser_executor.as_deref())
                    .await?;
            #[cfg(not(feature = "browser-native"))]
            let http_resp = crate::browser::browser_fetch(url, &self.config, None, pool).await?;
            let (crawl_resp, _extras) = Self::browser_http_to_crawl(http_resp);
            return Ok((crawl_resp, true));
        }

        let dispatch = self.config.dispatch.as_ref();
        let bypass = dispatch.and_then(|d| d.bypass.as_ref());
        let strategy = dispatch.map(|d| d.strategy).unwrap_or_default();
        let retry_policy: crate::types::DynRetryPolicy = dispatch
            .and_then(|d| d.retry_policy.clone())
            .unwrap_or_else(|| std::sync::Arc::new(crate::defaults::dispatch::SimpleRetryPolicy::new()));
        let budget: crate::types::DynEscalationBudget = dispatch
            .and_then(|d| d.escalation_budget.clone())
            .unwrap_or_else(|| std::sync::Arc::new(crate::defaults::dispatch::UnlimitedBudget));
        let max_total = dispatch.map(|d| d.max_total_attempts).unwrap_or(10).max(1);
        let antibot_strategy: Option<crate::types::antibot::DynAntibotStrategy> =
            dispatch.and_then(|d| d.antibot_strategy.clone());

        // ~keep Demote BrowserOnly when BrowserMode::Never so the original HTTP/WAF error survives.
        let effective_strategy = if strategy == crate::types::EscalationStrategy::BrowserOnly
            && self.config.browser.mode == crate::types::BrowserMode::Never
        {
            crate::types::EscalationStrategy::None
        } else {
            strategy
        };

        if matches!(effective_strategy, crate::types::EscalationStrategy::BypassFirst)
            && let Some(provider) = bypass
        {
            let bypass_resp = provider.fetch(url).await?;
            return Ok((
                crate::tower::CrawlResponse {
                    status: bypass_resp.status,
                    content_type: bypass_resp.content_type,
                    body: bypass_resp.body,
                    body_bytes: bypass_resp.body_bytes,
                    headers: bypass_resp.headers,
                },
                false,
            ));
        }

        let mut current_tier = crate::types::Tier::Http;
        let mut attempt: u32 = 0;
        // ~keep The global attempt cap guards against RetryPolicy implementations that never return Stop.
        let mut total_attempts: u32 = 0;
        let mut last_ok: Option<(crate::tower::CrawlResponse, bool)> = None;
        let mut last_err: Option<CrawlError> = None;
        let mut tiers_attempted: Vec<&'static str> = Vec::new();
        let mut last_escalation_reason: Option<&'static str> = None;
        let policy_name = retry_policy.name();
        let mut last_content_density: f32 = 0.0;

        loop {
            total_attempts += 1;
            // ~keep `max_total_attempts` is inclusive; reject only attempt max_total + 1.
            if total_attempts > max_total {
                tracing::warn!(
                    target: "crawlberg::dispatch",
                    url,
                    total_attempts,
                    max_total,
                    "max_total_attempts exceeded, force-returning current result"
                );
                return match last_ok {
                    Some((resp, browser_used)) => Ok((resp, browser_used)),
                    None => Err(last_err
                        .unwrap_or_else(|| CrawlError::Other("max_total_attempts exceeded with no result".into()))),
                };
            }
            tiers_attempted.push(Self::tier_name(current_tier));

            if let Some(strategy) = &antibot_strategy
                && let Err(e) = strategy.pre_request(url).await
            {
                tracing::warn!(
                    target: "crawlberg::antibot",
                    url,
                    error = %e,
                    "antibot pre_request hook failed; treating as transient error"
                );
                let outcome = crate::types::AttemptOutcome {
                    attempt,
                    url: std::sync::Arc::from(url),
                    status: None,
                    error: Some(CrawlError::Other(e.to_string())),
                    waf_signal: None,
                    body_size: 0,
                    content_density: 0.0,
                    bytes_transferred: None,
                    previous_tier: current_tier,
                };
                match retry_policy.decide(&outcome).await {
                    crate::types::RetryDirective::Stop => {
                        return Err(CrawlError::Other(format!("antibot pre_request failed: {e}")));
                    }
                    crate::types::RetryDirective::Retry { backoff_ms } => {
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    crate::types::RetryDirective::Escalate { reason } => {
                        if let Some(next) = Self::next_tier(current_tier, effective_strategy)
                            && budget.try_consume(Self::tier_cost_cents(next)).await.is_ok()
                        {
                            last_escalation_reason = Some(Self::escalation_reason_str(&reason));
                            current_tier = next;
                            attempt = 0;
                            continue;
                        }
                        return Err(CrawlError::Other(format!("antibot pre_request failed: {e}")));
                    }
                }
            }

            let tier_result = self.run_tier(current_tier, url).await;

            match tier_result {
                Ok((resp, browser_used)) => {
                    // ~keep Build a minimal HttpResponse here so WAF classification does not widen its trait surface.
                    let waf_classifier = dispatch.and_then(|d| d.waf_classifier.as_ref());
                    let http_resp_for_hooks = crate::http::HttpResponse {
                        status: resp.status,
                        content_type: resp.content_type.clone(),
                        body: resp.body.clone(),
                        body_bytes: resp.body_bytes.clone(),
                        headers: resp.headers.clone(),
                        final_url: String::new(),
                        browser_extras: None,
                    };
                    let waf_signal = waf_classifier.and_then(|c| match c.classify(&http_resp_for_hooks) {
                        Ok(sig) => sig,
                        Err(e) => {
                            tracing::warn!(
                                target: "crawlberg::waf",
                                error = %e,
                                "classify failed"
                            );
                            None
                        }
                    });

                    if let Some(strategy) = &antibot_strategy {
                        match strategy.post_response(&http_resp_for_hooks, waf_signal.as_ref()).await {
                            crate::types::Decision::Accept => {}
                            crate::types::Decision::Retry { backoff } => {
                                tokio::time::sleep(backoff).await;
                                attempt += 1;
                                continue;
                            }
                            crate::types::Decision::RotateProxy => {
                                tracing::warn!(
                                    target: "crawlberg::antibot",
                                    url,
                                    "RotateProxy decision received but proxy pool is not yet implemented; \
                                     treating as Accept"
                                );
                            }
                            crate::types::Decision::EscalateBrowser => {
                                let reason = crate::types::EscalationReason::AntibotEscalate;
                                if let Some(next) = Self::next_tier(current_tier, effective_strategy)
                                    && budget.try_consume(Self::tier_cost_cents(next)).await.is_ok()
                                {
                                    crate::telemetry::metrics::registry().backend_escalations_total.add(
                                        1,
                                        &[
                                            KeyValue::new("from_tier", Self::tier_name(current_tier)),
                                            KeyValue::new("to_tier", Self::tier_name(next)),
                                            KeyValue::new("reason", escalation_reason_label(&reason)),
                                        ],
                                    );
                                    last_escalation_reason = Some(Self::escalation_reason_str(&reason));
                                    current_tier = next;
                                    attempt = 0;
                                    continue;
                                }
                                Self::emit_dispatch_span(
                                    url,
                                    &tiers_attempted,
                                    last_escalation_reason,
                                    attempt,
                                    policy_name,
                                    last_content_density,
                                );
                                return Err(Self::escalation_reason_to_error(&reason, url));
                            }
                        }
                    }

                    last_ok = Some((resp.clone(), browser_used));

                    let density = content_density(&resp.body);
                    last_content_density = density;

                    let outcome = crate::types::AttemptOutcome {
                        attempt,
                        url: std::sync::Arc::from(url),
                        status: Some(resp.status),
                        error: None,
                        waf_signal,
                        body_size: resp.body.len(),
                        content_density: density,
                        bytes_transferred: Some(resp.body_bytes.len() as u64),
                        previous_tier: current_tier,
                    };
                    match retry_policy.decide(&outcome).await {
                        crate::types::RetryDirective::Stop => {
                            Self::emit_dispatch_span(
                                url,
                                &tiers_attempted,
                                last_escalation_reason,
                                attempt,
                                policy_name,
                                last_content_density,
                            );
                            return Ok((resp, browser_used));
                        }
                        crate::types::RetryDirective::Retry { backoff_ms } => {
                            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                            attempt += 1;
                            continue;
                        }
                        crate::types::RetryDirective::Escalate { reason } => {
                            if let Some(next) = Self::next_tier(current_tier, effective_strategy)
                                && budget.try_consume(Self::tier_cost_cents(next)).await.is_ok()
                            {
                                crate::telemetry::metrics::registry().backend_escalations_total.add(
                                    1,
                                    &[
                                        KeyValue::new("from_tier", Self::tier_name(current_tier)),
                                        KeyValue::new("to_tier", Self::tier_name(next)),
                                        KeyValue::new("reason", escalation_reason_label(&reason)),
                                    ],
                                );
                                last_escalation_reason = Some(Self::escalation_reason_str(&reason));
                                current_tier = next;
                                attempt = 0;
                                continue;
                            }
                            Self::emit_dispatch_span(
                                url,
                                &tiers_attempted,
                                last_escalation_reason,
                                attempt,
                                policy_name,
                                last_content_density,
                            );
                            return Err(Self::escalation_reason_to_error(&reason, url));
                        }
                    }
                }
                Err(err) => {
                    if self.config.soft_http_errors {
                        if matches!(err, CrawlError::NotFound(_)) {
                            return Ok((Self::synthesise_status(404), false));
                        }
                        if matches!(err, CrawlError::Forbidden(_) | CrawlError::WafBlocked { .. }) {
                            return Ok((Self::synthesise_status(403), false));
                        }
                    }

                    last_err = Some(err.clone());

                    // ~keep Error-arm WAF classification has vendor attribution but no classifier fingerprint.
                    // ~keep Use an empty fingerprint label instead of a sentinel to avoid extra Prometheus cardinality.
                    let waf_signal = match &err {
                        CrawlError::WafBlocked { vendor, .. } => Some(crate::types::WafSignal {
                            vendor: vendor.clone(),
                            fingerprint_id: String::new(),
                            weight: 1.0,
                        }),
                        _ => None,
                    };

                    let outcome = crate::types::AttemptOutcome {
                        attempt,
                        url: std::sync::Arc::from(url),
                        status: None,
                        error: Some(err.clone()),
                        waf_signal,
                        body_size: 0,
                        content_density: 0.0,
                        bytes_transferred: None,
                        previous_tier: current_tier,
                    };
                    match retry_policy.decide(&outcome).await {
                        crate::types::RetryDirective::Stop => {
                            Self::emit_dispatch_span(
                                url,
                                &tiers_attempted,
                                last_escalation_reason,
                                attempt,
                                policy_name,
                                last_content_density,
                            );
                            return Err(err);
                        }
                        crate::types::RetryDirective::Retry { backoff_ms } => {
                            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                            attempt += 1;
                            continue;
                        }
                        crate::types::RetryDirective::Escalate { reason } => {
                            if let Some(next) = Self::next_tier(current_tier, effective_strategy)
                                && budget.try_consume(Self::tier_cost_cents(next)).await.is_ok()
                            {
                                crate::telemetry::metrics::registry().backend_escalations_total.add(
                                    1,
                                    &[
                                        KeyValue::new("from_tier", Self::tier_name(current_tier)),
                                        KeyValue::new("to_tier", Self::tier_name(next)),
                                        KeyValue::new("reason", escalation_reason_label(&reason)),
                                    ],
                                );
                                last_escalation_reason = Some(Self::escalation_reason_str(&reason));
                                current_tier = next;
                                attempt = 0;
                                continue;
                            }
                            Self::emit_dispatch_span(
                                url,
                                &tiers_attempted,
                                last_escalation_reason,
                                attempt,
                                policy_name,
                                last_content_density,
                            );
                            return Err(err);
                        }
                    }
                }
            }
        }
    }

    /// Dispatch a single fetch attempt to the given tier.
    ///
    /// Returns `(CrawlResponse, browser_used)` or a `CrawlError`.
    #[cfg(not(target_arch = "wasm32"))]
    async fn run_tier(
        &self,
        tier: crate::types::Tier,
        url: &str,
    ) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        match tier {
            crate::types::Tier::Http => {
                let client = crate::http::build_client(&self.config)?;
                let mut service = self.build_service(&client);
                use tower::Service;
                let mut req = CrawlRequest::new(url);
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
                        CrawlError::InvalidConfig("escalation to Bypass tier but no bypass provider configured".into())
                    })?;
                let bypass_resp = provider.fetch(url).await?;
                Ok((
                    crate::tower::CrawlResponse {
                        status: bypass_resp.status,
                        content_type: bypass_resp.content_type,
                        body: bypass_resp.body,
                        body_bytes: bypass_resp.body_bytes,
                        headers: bypass_resp.headers,
                    },
                    false,
                ))
            }
            crate::types::Tier::Browser => {
                #[cfg(feature = "browser")]
                {
                    let pool = self.config.browser_pool.as_deref();
                    #[cfg(feature = "browser-native")]
                    let http_resp = crate::browser::browser_fetch(
                        url,
                        &self.config,
                        None,
                        pool,
                        self.native_browser_executor.as_deref(),
                    )
                    .await?;
                    #[cfg(not(feature = "browser-native"))]
                    let http_resp = crate::browser::browser_fetch(url, &self.config, None, pool).await?;
                    let (crawl_resp, _extras) = Self::browser_http_to_crawl(http_resp);
                    Ok((crawl_resp, true))
                }
                #[cfg(not(feature = "browser"))]
                Err(CrawlError::Unsupported(
                    "Browser tier requires the 'browser' feature".into(),
                ))
            }
        }
    }

    /// Convert an `HttpResponse` (from the browser path) into the `CrawlResponse`
    /// shape expected by the extraction pipeline.
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser"))]
    fn browser_http_to_crawl(
        r: crate::http::HttpResponse,
    ) -> (crate::tower::CrawlResponse, Option<crate::http::BrowserExtras>) {
        let extras = r.browser_extras;
        (
            crate::tower::CrawlResponse {
                status: r.status,
                content_type: r.content_type,
                body: r.body,
                body_bytes: r.body_bytes,
                headers: std::collections::HashMap::new(),
            },
            extras,
        )
    }

    /// Synthesise a minimal response with the given HTTP status (empty body).
    ///
    /// Used by `soft_http_errors` to surface 4xx responses as `ScrapeResult`
    /// records rather than `CrawlError`.
    #[cfg(not(target_arch = "wasm32"))]
    fn synthesise_status(status: u16) -> crate::tower::CrawlResponse {
        crate::tower::CrawlResponse {
            status,
            content_type: String::new(),
            body: String::new(),
            body_bytes: Vec::new(),
            headers: std::collections::HashMap::new(),
        }
    }

    /// Convert an [`crate::types::EscalationReason`] from a terminal success-path
    /// `Escalate` directive into the most specific available [`CrawlError`].
    ///
    /// Called when the policy signals `Escalate` on a 2xx response (soft-block /
    /// WAF interstitial) but no higher tier is available or the budget is exhausted.
    /// Returning an error prevents the challenge-page body from reaching callers.
    #[cfg(not(target_arch = "wasm32"))]
    fn escalation_reason_to_error(reason: &crate::types::EscalationReason, url: &str) -> CrawlError {
        use crate::types::EscalationReason;
        match reason {
            EscalationReason::WafBlocked { vendor } => CrawlError::WafBlocked {
                vendor: vendor.clone(),
                message: format!("waf/blocked: {vendor} detected at {url}"),
            },
            EscalationReason::SoftBlock => CrawlError::Forbidden(format!("soft_block: {url}")),
            EscalationReason::RenderNeeded => {
                CrawlError::Unsupported(format!("js_render_needed but no browser tier available: {url}"))
            }
            EscalationReason::OriginUnreliable => {
                CrawlError::ServerError(format!("origin_unreliable and no escalation target: {url}"))
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
    fn next_tier(
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
    const fn tier_cost_cents(tier: crate::types::Tier) -> u32 {
        match tier {
            crate::types::Tier::Http => 0,
            crate::types::Tier::Bypass | crate::types::Tier::Browser => 1,
        }
    }

    /// Stable lowercase name for a tier, used in span attributes and OTel labels.
    #[cfg(not(target_arch = "wasm32"))]
    const fn tier_name(tier: crate::types::Tier) -> &'static str {
        match tier {
            crate::types::Tier::Http => "http",
            crate::types::Tier::Bypass => "bypass",
            crate::types::Tier::Browser => "browser",
        }
    }

    /// Stable lowercase string for an escalation reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn escalation_reason_str(reason: &crate::types::EscalationReason) -> &'static str {
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
    fn emit_dispatch_span(
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
    #[tracing::instrument(name = "crawl.engine.scrape", skip(self), fields(url.full = tracing::field::Empty))]
    pub async fn scrape(&self, url: &str) -> Result<ScrapeResult, CrawlError> {
        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        self.config.validate()?;

        // ~keep Short-circuit native BrowserMode::Always so browser_extras survive fetch_response conversion.
        #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
        if self.config.browser.mode == crate::types::BrowserMode::Always
            && self.config.browser.backend == crate::types::BrowserBackend::Native
        {
            let native_executor = self.native_browser_executor.as_deref().ok_or_else(|| {
                CrawlError::BrowserError("native browser executor is not available for BrowserBackend::Native".into())
            })?;
            let mut http_resp =
                crate::native_browser::native_browser_fetch(url, &self.config, None, native_executor).await?;
            let raw_extras = http_resp.browser_extras.take();
            let crawl_resp = crate::tower::CrawlResponse {
                status: http_resp.status,
                content_type: http_resp.content_type,
                body: http_resp.body,
                body_bytes: http_resp.body_bytes,
                headers: std::collections::HashMap::new(),
            };
            let mut result = crate::scrape::scrape_from_crawl_response(url, &crawl_resp, &self.config).await?;
            result.browser_used = true;
            if let Some(ex) = raw_extras {
                result.browser = Some(crate::types::BrowserExtras {
                    eval_result: ex.eval_result,
                    network_events: ex.network_events,
                    cookies: ex.cookies,
                });
            }
            return Ok(result);
        }

        #[cfg(not(target_arch = "wasm32"))]
        let (final_url, response, browser_used_for_fetch) = {
            use crawl_loop::follow_redirects;

            let max_redirects = self.config.max_redirects;
            let outcome = follow_redirects(self, url, max_redirects).await?;

            // ~keep Synthesized empty 4xx responses return minimal results instead of parsing an empty body as HTML.
            let status = outcome.final_response.status;
            if matches!(status, 404 | 403) && outcome.final_response.body.is_empty() && self.config.soft_http_errors {
                return Ok(ScrapeResult {
                    status_code: status,
                    final_url: outcome.final_url,
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
                    downloaded_document: None,
                    browser: None,
                });
            }
            if outcome.final_response.status == 404
                && outcome.final_response.body.is_empty()
                && outcome.redirect_count > 0
            {
                return Ok(ScrapeResult {
                    status_code: 404,
                    final_url: outcome.final_url,
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
                    downloaded_document: None,
                    browser: None,
                });
            }
            (outcome.final_url, outcome.final_response, outcome.browser_used)
        };

        #[cfg(target_arch = "wasm32")]
        let (final_url, response, browser_used_for_fetch) = {
            let client = crate::http::build_client(&self.config)?;
            let resp =
                crate::http::fetch_with_retry(url, &self.config, &std::collections::HashMap::new(), &client).await?;
            // ~keep On wasm, browser fetch follows redirects; `resp.final_url` is the post-redirect URL.
            let post_redirect_url = resp.final_url.clone();
            let crawl_resp = crate::tower::CrawlResponse {
                status: resp.status,
                content_type: resp.content_type,
                body: resp.body,
                body_bytes: resp.body_bytes,
                headers: resp.headers,
            };
            (post_redirect_url, crawl_resp, false)
        };

        let mut result = crate::scrape::scrape_from_crawl_response(&final_url, &response, &self.config).await?;
        result.browser_used = browser_used_for_fetch;

        // ~keep Without the browser feature, BrowserMode::Always still reports browser_used for binding parity.
        #[cfg(not(feature = "browser"))]
        if self.config.browser.mode == crate::types::BrowserMode::Always {
            result.browser_used = true;
        }

        Ok(result)
    }

    /// Execute browser actions on a single page.
    ///
    /// The public API is always available. Runtime execution depends on the
    /// configured browser backend and the browser backend features compiled
    /// into the crate.
    // ~keep `actions` may carry user-typed form text (TypeText), so it is skipped
    // ~keep rather than recorded; only its length is cheap and safe to trace.
    #[tracing::instrument(
        name = "crawl.engine.interact",
        skip(self, actions),
        fields(url.full = tracing::field::Empty, action_count = actions.len())
    )]
    pub async fn interact(
        &self,
        url: &str,
        actions: &[crate::interact::PageAction],
    ) -> Result<InteractionResult, CrawlError> {
        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        crate::interact::run(self, url, actions).await
    }

    /// Discover all pages on a website by following links and sitemaps.
    #[tracing::instrument(name = "crawl.engine.map", skip(self), fields(url.full = tracing::field::Empty))]
    pub async fn map(&self, url: &str) -> Result<MapResult, CrawlError> {
        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        self.config.validate()?;
        crate::map::map(url, &self.config).await
    }
}

/// Wasm-specific sequential multi-page crawl implementations.
///
/// The native crawl loop uses `tokio::spawn`, `JoinSet`, and `Semaphore` which do not
/// compile to `wasm32-unknown-unknown`. These implementations drive the same BFS/DFS/
/// strategy logic sequentially using `.await` only — no concurrency primitives.
#[cfg(target_arch = "wasm32")]
impl CrawlEngine {
    /// Normalize a URL for deduplication on wasm.
    ///
    /// Strips query parameters and fragment, removes trailing slash (except root).
    /// Mirrors `normalize::normalize_url_for_dedup` which is cfg-gated to non-wasm.
    fn wasm_dedup_key(raw: &str) -> String {
        if let Ok(mut u) = url::Url::parse(raw) {
            u.set_fragment(None);
            u.set_query(None);
            let path = u.path().to_owned();
            if path.len() > 1 && path.ends_with('/') {
                u.set_path(&path[..path.len() - 1]);
            }
            u.to_string()
        } else {
            raw.to_owned()
        }
    }

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
        }
    }

    /// Compile regex patterns for path filtering, returning an error on invalid patterns.
    fn compile_path_regexes(patterns: &[String]) -> Result<Vec<regex::Regex>, CrawlError> {
        patterns
            .iter()
            .map(|pat| {
                regex::Regex::new(pat).map_err(|e| CrawlError::Other(format!("invalid regex pattern \"{pat}\": {e}")))
            })
            .collect()
    }

    /// Crawl a website starting from `url`.
    ///
    /// Implements a sequential BFS/strategy-driven crawl loop. Follows links discovered
    /// during scraping and applies `max_depth`, `max_pages`, `stay_on_domain`,
    /// `allow_subdomains`, `include_paths`, `exclude_paths`, and the configured
    /// `CrawlStrategy`. No concurrency primitives are used — each page is awaited
    /// sequentially, which is correct for the wasm single-threaded executor.
    #[tracing::instrument(name = "crawl.engine.crawl", skip(self), fields(url.full = tracing::field::Empty))]
    pub async fn crawl(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        use std::collections::HashSet;

        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        self.config.validate()?;

        let parsed_seed = url::Url::parse(url).map_err(|e| CrawlError::Other(format!("invalid URL: {e}")))?;
        let base_host = parsed_seed.host_str().unwrap_or("").to_owned();
        let base_host_suffix = format!(".{base_host}");

        let max_depth = self.config.max_depth.unwrap_or(usize::MAX);
        let max_pages = self.config.max_pages.unwrap_or(usize::MAX);

        let exclude_regexes = Self::compile_path_regexes(&self.config.exclude_paths)?;
        let include_regexes = Self::compile_path_regexes(&self.config.include_paths)?;

        let mut seen: HashSet<String> = HashSet::new();

        let seed_dedup = Self::wasm_dedup_key(url);
        seen.insert(seed_dedup.clone());
        let _ = self.frontier.mark_seen(&seed_dedup).await;

        let mut working_set: Vec<FrontierEntry> = vec![FrontierEntry {
            url: url.to_owned(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        }];

        let mut pages: Vec<CrawlPageResult> = Vec::new();
        let mut normalized_urls: Vec<String> = Vec::new();
        let mut redirect_count: usize = 0;
        let mut was_skipped = false;
        let mut pages_failed: usize = 0;
        let mut urls_discovered: usize = 0;
        let mut urls_filtered: usize = 0;
        let mut crawl_error: Option<String> = None;
        let mut final_url = url.to_owned();

        while !working_set.is_empty() {
            let stats = CrawlStats {
                pages_crawled: pages.len(),
                pages_failed,
                urls_discovered,
                urls_filtered,
                elapsed: std::time::Duration::ZERO,
            };
            if !self.strategy.should_continue(&stats) {
                break;
            }
            if pages.len() >= max_pages {
                break;
            }

            let Some(idx) = self.strategy.select_next(&working_set) else {
                break;
            };
            let entry = working_set.swap_remove(idx);

            if let Ok(parsed) = url::Url::parse(&entry.url) {
                let path = parsed.path();
                if !exclude_regexes.is_empty() && exclude_regexes.iter().any(|re| re.is_match(path)) {
                    urls_filtered += 1;
                    continue;
                }
                if !include_regexes.is_empty() && entry.depth > 0 && !include_regexes.iter().any(|re| re.is_match(path))
                {
                    urls_filtered += 1;
                    continue;
                }
            }

            match self.page_budget.check().await {
                Ok(()) => {}
                Err(crate::budget::BudgetError::Exhausted) => {
                    tracing::info!(target: "crawlberg.budget", "page budget exhausted");
                    break;
                }
                Err(crate::budget::BudgetError::Backend(msg)) => {
                    // ~keep Degraded, not data loss: the crawl still returns whatever pages
                    // ~keep were already fetched, so this belongs at WARN, not ERROR.
                    tracing::warn!(target: "crawlberg.budget", error = %msg, "budget backend error; treating as exhausted");
                    break;
                }
            }

            let scrape = match self.scrape(&entry.url).await {
                Ok(s) => s,
                Err(e) => {
                    pages_failed += 1;
                    let error_msg = e.to_string();
                    self.event_emitter
                        .on_error(&crate::traits::ErrorEvent {
                            url: entry.url.clone(),
                            error: error_msg.clone(),
                        })
                        .await;
                    let _ = self.store.store_error(&entry.url, &e).await;
                    if entry.depth == 0 {
                        crawl_error = Some(error_msg);
                    }
                    continue;
                }
            };

            if entry.depth == 0 {
                final_url = scrape.final_url.clone();
                if scrape.final_url != entry.url {
                    redirect_count += 1;
                }
            }

            if scrape.was_skipped || scrape.is_pdf {
                was_skipped = true;
            }

            let in_doc_context = entry.doc_depth > 0;
            let page_is_skipped_wasm = scrape.was_skipped || scrape.is_pdf;
            let should_discover_wasm = entry.depth < max_depth
                && !page_is_skipped_wasm
                && (!in_doc_context || self.config.follow_document_urls);
            if should_discover_wasm {
                // ~keep A single page can carry unbounded link fan-out (e.g. a sitemap-like page
                // ~keep with a million anchors); cap per-page discovery so one page cannot blow
                // ~keep up frontier memory in a single iteration.
                const MAX_LINKS_PER_PAGE: usize = 10_000;
                if scrape.links.len() > MAX_LINKS_PER_PAGE {
                    tracing::warn!(
                        target: "crawlberg.frontier",
                        url = %entry.url,
                        link_count = scrape.links.len(),
                        cap = MAX_LINKS_PER_PAGE,
                        "page link fan-out exceeds cap, truncating discovered links"
                    );
                }
                for link in scrape.links.iter().take(MAX_LINKS_PER_PAGE) {
                    let is_doc_link = link.link_type == LinkType::Document;

                    if link.link_type != LinkType::Internal && !is_doc_link {
                        continue;
                    }

                    if is_doc_link && entry.doc_depth > 0 {
                        if !self.config.follow_document_urls {
                            continue;
                        }
                        let child_doc_depth = entry.doc_depth + 1;
                        if let Some(max_doc_depth) = self.config.document_url_depth
                            && child_doc_depth > max_doc_depth
                        {
                            continue;
                        }
                    }

                    let link_url = crate::normalize::strip_fragment(&link.url);

                    if self.config.stay_on_domain
                        && let Ok(lu) = url::Url::parse(&link_url)
                    {
                        let link_host = lu.host_str().unwrap_or("");
                        if link_host != base_host
                            && (!self.config.allow_subdomains || !link_host.ends_with(&base_host_suffix))
                        {
                            continue;
                        }
                    }

                    let dedup_key = Self::wasm_dedup_key(&link_url);
                    if !seen.contains(&dedup_key) {
                        seen.insert(dedup_key.clone());
                        let _ = self.frontier.mark_seen(&dedup_key).await;
                        let child_depth = entry.depth + 1;
                        let child_doc_depth: u32 = if is_doc_link { entry.doc_depth + 1 } else { 0 };
                        let priority = self.strategy.score_url(&link_url, child_depth);
                        working_set.push(FrontierEntry {
                            url: link_url.clone(),
                            depth: child_depth,
                            doc_depth: child_doc_depth,
                            priority,
                        });
                        urls_discovered += 1;
                        self.event_emitter.on_discovered(&link_url, child_depth).await;
                    }
                }
            }

            let page_url = scrape.final_url.clone();
            let page = Self::scrape_to_crawl_page(scrape, &page_url, entry.depth, &base_host);

            let page = match self.content_filter.filter(page).await? {
                Some(filtered) => filtered,
                None => {
                    urls_filtered += 1;
                    continue;
                }
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

            normalized_urls.push(crate::normalize::normalize_url(&page.url));
            pages.push(page);
        }

        let _ = self
            .store
            .on_complete(&CrawlStats {
                pages_crawled: pages.len(),
                pages_failed,
                urls_discovered,
                urls_filtered,
                elapsed: std::time::Duration::ZERO,
            })
            .await;
        self.event_emitter
            .on_complete(&crate::traits::CompleteEvent {
                pages_crawled: pages.len(),
            })
            .await;

        if pages.len() > max_pages {
            pages.truncate(max_pages);
        }

        let stayed_on_domain = pages.iter().all(|p| p.stayed_on_domain);
        Ok(CrawlResult::new(
            pages,
            final_url,
            redirect_count,
            was_skipped,
            crawl_error,
            Vec::new(),
            stayed_on_domain,
            normalized_urls,
        ))
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Minimal `Subscriber` that captures every field recorded on any span,
    /// used to assert on `#[tracing::instrument]` field values without adding
    /// a `tracing-subscriber` dev-dependency.
    #[derive(Default)]
    struct FieldCapture(std::sync::Mutex<Vec<(String, String)>>);

    impl FieldCapture {
        fn visitor(&self) -> impl tracing::field::Visit + '_ {
            struct V<'a>(&'a FieldCapture);
            impl tracing::field::Visit for V<'_> {
                fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    self.0
                        .0
                        .lock()
                        .unwrap()
                        .push((field.name().to_string(), format!("{value:?}")));
                }
            }
            V(self)
        }
    }

    struct CapturingSubscriber(std::sync::Arc<FieldCapture>);

    impl tracing::Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            span.record(&mut self.0.visitor());
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            values.record(&mut self.0.visitor());
        }
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, _event: &tracing::Event<'_>) {}
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// Regression test: `CrawlEngine::scrape`'s `crawl.engine.scrape` span must record
    /// the `url.full` field with credentials redacted (see `crate::net::redact_url_credentials`),
    /// regardless of whether the fetch itself succeeds.
    #[tokio::test]
    async fn scrape_span_records_redacted_url() {
        let captured = std::sync::Arc::new(FieldCapture::default());
        let subscriber = CapturingSubscriber(captured.clone());
        let engine = CrawlEngine::builder().build().expect("engine build must not fail");

        let _guard = tracing::subscriber::set_default(subscriber);
        let _ = engine.scrape("http://user:hunter2@127.0.0.1:1/").await;
        drop(_guard);

        let fields = captured.0.lock().unwrap();
        let (_, value) = fields
            .iter()
            .find(|(name, _)| name == "url.full")
            .unwrap_or_else(|| panic!("expected a url.full span field to be recorded, got {fields:?}"));
        assert!(
            !value.contains("hunter2"),
            "redacted url.full must not leak the password, got {value}"
        );
        assert!(
            value.contains("127.0.0.1"),
            "redacted url.full should retain the host, got {value}"
        );
    }

    /// Verify that a connection-refused error propagates with [network:connection] tag
    /// rather than being swallowed by browser fallback. The engine's BrowserMode::Auto
    /// arm must not include Connection errors.
    #[tokio::test]
    async fn connection_refused_propagates_network_tag() {
        use crate::error::classify_reqwest_error;
        use std::time::Duration;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .expect("client build must not fail");

        let raw_err = client
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("expected connection error");

        let err = classify_reqwest_error(&raw_err);
        let msg = err.to_string();
        assert!(
            msg.contains("[network:connection]"),
            "expected [network:connection] in '{msg}'"
        );
        assert!(
            matches!(err, CrawlError::Connection(_)),
            "expected CrawlError::Connection, got {err:?}"
        );
    }

    /// Verify that a DNS resolution failure propagates with [network:dns] tag.
    #[tokio::test]
    async fn dns_failure_propagates_network_tag() {
        use crate::error::classify_reqwest_error;
        use std::time::Duration;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(1000))
            .build()
            .expect("client build must not fail");

        let raw_err = client
            .get("http://this-host-does-not-exist-crawlberg-engine-test.invalid/")
            .send()
            .await
            .expect_err("expected dns error");

        let err = classify_reqwest_error(&raw_err);
        let msg = err.to_string();
        assert!(msg.contains("[network:dns]"), "expected [network:dns] in '{msg}'");
        assert!(
            matches!(err, CrawlError::Dns(_)),
            "expected CrawlError::Dns, got {err:?}"
        );
    }
}
