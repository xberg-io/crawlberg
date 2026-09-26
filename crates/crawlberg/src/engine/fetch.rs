//! The single-fetch entry point: tier dispatch, escalation, and browser fallback.

#![cfg(not(target_arch = "wasm32"))]

use opentelemetry::KeyValue;

use super::CrawlEngine;
use super::dispatch::{content_density, escalation_reason_label};
use crate::error::CrawlError;
use crate::types::*;

/// What the dispatch loop should do once a step has had its say.
enum LoopStep {
    /// Carry on with the current attempt.
    Proceed,
    /// Begin the next attempt without running a tier; the loop state is already advanced.
    Restart,
    /// Finish the fetch with this result.
    Done(Result<(crate::tower::CrawlResponse, bool), CrawlError>),
}

/// The dispatch policy in force for one [`CrawlEngine::fetch_response`] call.
///
/// Resolved once up front rather than per attempt: every field is an `Arc` clone or a
/// copy of a config value, and the loop below reads them on every iteration.
struct DispatchPlan {
    retry_policy: DynRetryPolicy,
    budget: DynEscalationBudget,
    waf_classifier: Option<DynWafClassifier>,
    antibot_strategy: Option<crate::types::antibot::DynAntibotStrategy>,
    effective_strategy: EscalationStrategy,
    max_total: u32,
    policy_name: &'static str,
}

/// What the configured hooks are shown of one successful attempt.
struct HookView {
    response: Option<crate::http::HttpResponse>,
    waf_signal: Option<WafSignal>,
}

/// The mutable state carried across the attempts of one fetch.
struct AttemptState {
    current_tier: Tier,
    attempt: u32,
    /// ~keep The global attempt cap guards against RetryPolicy implementations that never return Stop.
    total_attempts: u32,
    last_ok: Option<(crate::tower::CrawlResponse, bool)>,
    last_err: Option<CrawlError>,
    tiers_attempted: Vec<&'static str>,
    last_escalation_reason: Option<&'static str>,
    last_content_density: f32,
}

impl DispatchPlan {
    fn from_config(config: &CrawlConfig) -> Self {
        let dispatch = config.dispatch.as_ref();
        let strategy = dispatch.map(|d| d.strategy).unwrap_or_default();
        let configured_retry_policy = dispatch.and_then(|d| d.retry_policy.clone());
        // ~keep `from_config`, not `new()`: the default policy must derive `max_retries` from
        // ~keep `config.retry_count` (and the backoff bounds from `retry_initial_delay_ms`/
        // ~keep `retry_max_delay_ms`), or a caller's `retry_count=0` is silently overridden by
        // ~keep `new()`'s hardcoded 3 retries — see crawlberg#68.
        let using_default_retry_policy = configured_retry_policy.is_none();
        let retry_policy: DynRetryPolicy = configured_retry_policy
            .unwrap_or_else(|| std::sync::Arc::new(crate::defaults::dispatch::SimpleRetryPolicy::from_config(config)));
        let policy_name = retry_policy.name();

        // ~keep Demote BrowserOnly when BrowserMode::Never so the original HTTP/WAF error survives.
        let effective_strategy =
            if strategy == EscalationStrategy::BrowserOnly && config.browser.mode == BrowserMode::Never {
                EscalationStrategy::None
            } else {
                strategy
            };

        Self {
            retry_policy,
            budget: dispatch
                .and_then(|d| d.escalation_budget.clone())
                .unwrap_or_else(|| std::sync::Arc::new(crate::defaults::dispatch::UnlimitedBudget)),
            waf_classifier: dispatch.and_then(|d| d.waf_classifier.clone()),
            antibot_strategy: dispatch.and_then(|d| d.antibot_strategy.clone()),
            effective_strategy,
            // ~keep When the default `SimpleRetryPolicy::from_config` is in play, `max_total`
            // ~keep must never cap attempts below what `retry_count` asked for — the default
            // ~keep `max_total_attempts` (10) would otherwise silently truncate a configured
            // ~keep `retry_count` above 9. A caller-supplied custom `retry_policy` keeps the
            // ~keep floor as-is: `max_total_attempts` is its safety valve against a policy that
            // ~keep never returns `Stop`, and `retry_count` has no defined meaning to it.
            max_total: {
                let configured = dispatch.map(|d| d.max_total_attempts).unwrap_or(10).max(1);
                if using_default_retry_policy {
                    let retry_attempts = u32::try_from(config.retry_count).unwrap_or(u32::MAX).saturating_add(1);
                    configured.max(retry_attempts)
                } else {
                    configured
                }
            },
            policy_name,
        }
    }

    /// Present one successful attempt to whichever hooks are configured to see it.
    fn inspect(&self, resp: &crate::tower::CrawlResponse) -> HookView {
        // ~keep Only materialize this (two full-body clones + a header-map deep copy) when a
        // classifier or an antibot strategy is actually configured to consume it; both are
        // None under default config, and this runs on every successful fetch attempt.
        let needs_response = self.waf_classifier.is_some() || self.antibot_strategy.is_some();
        // ~keep Build a minimal HttpResponse here so WAF classification does not widen its trait surface.
        let response = needs_response.then(|| crate::http::HttpResponse {
            status: resp.status,
            content_type: resp.content_type.clone(),
            body: resp.body.clone(),
            body_bytes: resp.body_bytes.clone(),
            headers: resp.headers.clone(),
            final_url: String::new(),
            browser_extras: None,
            screenshot: None,
        });

        let waf_signal = match (self.waf_classifier.as_ref(), response.as_ref()) {
            (Some(c), Some(h)) => match c.classify(h) {
                Ok(sig) => sig,
                Err(e) => {
                    tracing::warn!(
                        target: "crawlberg::waf",
                        error = %e,
                        "classify failed"
                    );
                    None
                }
            },
            _ => None,
        };

        HookView { response, waf_signal }
    }
}

impl AttemptState {
    fn new() -> Self {
        Self {
            current_tier: Tier::Http,
            attempt: 0,
            total_attempts: 0,
            last_ok: None,
            last_err: None,
            tiers_attempted: Vec::new(),
            last_escalation_reason: None,
            last_content_density: 0.0,
        }
    }

    /// The tier to escalate to, if the strategy offers one and the budget affords it.
    async fn affordable_next_tier(&self, plan: &DispatchPlan) -> Option<Tier> {
        let next = CrawlEngine::next_tier(self.current_tier, plan.effective_strategy)?;
        plan.budget.try_consume(CrawlEngine::tier_cost_cents(next)).await.ok()?;
        Some(next)
    }

    /// Point the loop at `next`, restarting its per-tier attempt counter.
    fn escalate_to(&mut self, next: Tier, reason: &EscalationReason) {
        self.last_escalation_reason = Some(CrawlEngine::escalation_reason_str(reason));
        self.current_tier = next;
        self.attempt = 0;
    }

    fn report_dispatch(&self, url: &str, plan: &DispatchPlan) {
        CrawlEngine::emit_dispatch_span(
            url,
            &self.tiers_attempted,
            self.last_escalation_reason,
            self.attempt,
            plan.policy_name,
            self.last_content_density,
        );
    }

    /// The result to hand back when the global attempt cap is reached.
    fn force_return(self, url: &str, max_total: u32) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        tracing::warn!(
            target: "crawlberg::dispatch",
            url,
            total_attempts = self.total_attempts,
            max_total,
            "max_total_attempts exceeded, force-returning current result"
        );
        match self.last_ok {
            Some((resp, browser_used)) => Ok((resp, browser_used)),
            None => Err(self
                .last_err
                .unwrap_or_else(|| CrawlError::other("max_total_attempts exceeded with no result"))),
        }
    }
}

impl CrawlEngine {
    /// Fetch a URL through the appropriate path (Tower stack or browser) and
    /// return the `CrawlResponse` together with a flag indicating whether the
    /// browser was used.
    ///
    /// This is intentionally `#[cfg(not(target_arch = "wasm32"))]`-only: wasm
    /// has its own simpler inline path inside `scrape`.
    ///
    /// `origin_host` is the host that started the redirect chain `url` belongs to, or
    /// `None` when `url` is itself the origin. It scopes configured credentials to that
    /// host — see [`crate::tower::CrawlRequest::is_on_origin_host`].
    pub(super) async fn fetch_response(
        &self,
        url: &str,
        origin_host: Option<&str>,
    ) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        #[cfg(feature = "browser")]
        if matches!(self.config.browser.mode, BrowserMode::Always | BrowserMode::Stealth) {
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
            return Ok((crawl_resp, true));
        }

        let plan = DispatchPlan::from_config(&self.config);

        if matches!(plan.effective_strategy, EscalationStrategy::BypassFirst)
            && let Some(provider) = self.config.dispatch.as_ref().and_then(|d| d.bypass.as_ref())
        {
            let bypass_resp = provider.fetch(url).await?;
            return Ok((
                crate::tower::CrawlResponse {
                    status: bypass_resp.status,
                    content_type: bypass_resp.content_type,
                    body: bypass_resp.body,
                    body_bytes: bypass_resp.body_bytes,
                    headers: bypass_resp.headers,
                    landed: None,
                },
                false,
            ));
        }

        self.run_dispatch_loop(url, origin_host, &plan).await
    }

    /// Attempt the fetch, retrying and escalating tiers until the policy says stop.
    async fn run_dispatch_loop(
        &self,
        url: &str,
        origin_host: Option<&str>,
        plan: &DispatchPlan,
    ) -> Result<(crate::tower::CrawlResponse, bool), CrawlError> {
        let mut state = AttemptState::new();

        loop {
            state.total_attempts += 1;
            // ~keep `max_total_attempts` is inclusive; reject only attempt max_total + 1.
            if state.total_attempts > plan.max_total {
                return state.force_return(url, plan.max_total);
            }
            state.tiers_attempted.push(Self::tier_name(state.current_tier));

            match self.antibot_pre_request(url, plan, &mut state).await {
                LoopStep::Proceed => {}
                LoopStep::Restart => continue,
                LoopStep::Done(result) => return result,
            }

            let step = match self.run_tier(state.current_tier, url, origin_host).await {
                Ok(fetched) => self.handle_tier_success(url, fetched, plan, &mut state).await,
                Err(err) => self.handle_tier_error(url, err, plan, &mut state).await,
            };
            match step {
                LoopStep::Proceed | LoopStep::Restart => continue,
                LoopStep::Done(result) => return result,
            }
        }
    }

    /// Run the antibot pre-request hook, if one is configured, and act on a failure.
    async fn antibot_pre_request(&self, url: &str, plan: &DispatchPlan, state: &mut AttemptState) -> LoopStep {
        let Some(strategy) = plan.antibot_strategy.as_ref() else {
            return LoopStep::Proceed;
        };
        let Err(e) = strategy.pre_request(url).await else {
            return LoopStep::Proceed;
        };

        tracing::warn!(
            target: "crawlberg::antibot",
            url,
            error = %e,
            "antibot pre_request hook failed; treating as transient error"
        );
        let outcome = AttemptOutcome {
            attempt: state.attempt,
            url: std::sync::Arc::from(url),
            status: None,
            error: Some(CrawlError::other(e.to_string())),
            waf_signal: None,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: state.current_tier,
        };
        match plan.retry_policy.decide(&outcome).await {
            RetryDirective::Stop => LoopStep::Done(Err(CrawlError::other(format!("antibot pre_request failed: {e}")))),
            RetryDirective::Retry { backoff_ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                state.attempt += 1;
                LoopStep::Restart
            }
            RetryDirective::Escalate { reason } => {
                if let Some(next) = state.affordable_next_tier(plan).await {
                    // ~keep No `backend_escalations_total` here, unlike the two arms below: this
                    // ~keep escalation happens before any tier produced a response, and the metric
                    // ~keep reports transitions observed from one. Emitting it here would change
                    // ~keep what the counter has always measured.
                    state.escalate_to(next, &reason);
                    return LoopStep::Restart;
                }
                LoopStep::Done(Err(CrawlError::other(format!("antibot pre_request failed: {e}"))))
            }
        }
    }

    /// Decide what a successful tier attempt means for the loop.
    async fn handle_tier_success(
        &self,
        url: &str,
        fetched: (crate::tower::CrawlResponse, bool),
        plan: &DispatchPlan,
        state: &mut AttemptState,
    ) -> LoopStep {
        let (resp, browser_used) = fetched;
        let hooks = plan.inspect(&resp);

        if let Some(step) = self.apply_antibot_decision(url, &hooks, plan, state).await {
            return step;
        }

        let density = content_density(&resp.body);
        state.last_content_density = density;

        let outcome = AttemptOutcome {
            attempt: state.attempt,
            url: std::sync::Arc::from(url),
            status: Some(resp.status),
            error: None,
            waf_signal: hooks.waf_signal,
            body_size: resp.body.len(),
            content_density: density,
            bytes_transferred: Some(resp.body_bytes.len() as u64),
            previous_tier: state.current_tier,
        };
        match plan.retry_policy.decide(&outcome).await {
            RetryDirective::Stop => {
                state.report_dispatch(url, plan);
                LoopStep::Done(Ok((resp, browser_used)))
            }
            RetryDirective::Retry { backoff_ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                state.attempt += 1;
                // ~keep Moves (not clones) `resp` into `last_ok`: it is only read on the rare
                // total_attempts > max_total bail-out, and this loop iteration has no other
                // use for `resp` after this point.
                state.last_ok = Some((resp, browser_used));
                LoopStep::Restart
            }
            RetryDirective::Escalate { reason } => {
                if let Some(next) = state.affordable_next_tier(plan).await {
                    Self::record_escalation(state.current_tier, next, &reason);
                    state.escalate_to(next, &reason);
                    state.last_ok = Some((resp, browser_used));
                    return LoopStep::Restart;
                }
                state.report_dispatch(url, plan);
                LoopStep::Done(Err(Self::escalation_reason_to_error(&reason, url)))
            }
        }
    }

    /// Consult the antibot strategy about a response, returning `None` to accept it.
    async fn apply_antibot_decision(
        &self,
        url: &str,
        hooks: &HookView,
        plan: &DispatchPlan,
        state: &mut AttemptState,
    ) -> Option<LoopStep> {
        let strategy = plan.antibot_strategy.as_ref()?;
        // ~keep `DispatchPlan::inspect` builds the response whenever `antibot_strategy` is Some.
        let response = hooks
            .response
            .as_ref()
            .expect("hook response is built when antibot_strategy is Some");

        match strategy.post_response(response, hooks.waf_signal.as_ref()).await {
            Decision::Accept => None,
            Decision::Retry { backoff } => {
                tokio::time::sleep(backoff).await;
                state.attempt += 1;
                Some(LoopStep::Restart)
            }
            Decision::RotateProxy => {
                tracing::warn!(
                    target: "crawlberg::antibot",
                    url,
                    "RotateProxy decision received but proxy pool is not yet implemented; \
                     treating as Accept"
                );
                None
            }
            Decision::EscalateBrowser => {
                let reason = EscalationReason::AntibotEscalate;
                if let Some(next) = state.affordable_next_tier(plan).await {
                    Self::record_escalation(state.current_tier, next, &reason);
                    state.escalate_to(next, &reason);
                    return Some(LoopStep::Restart);
                }
                state.report_dispatch(url, plan);
                Some(LoopStep::Done(Err(Self::escalation_reason_to_error(&reason, url))))
            }
        }
    }

    /// Decide what a failed tier attempt means for the loop.
    async fn handle_tier_error(
        &self,
        url: &str,
        err: CrawlError,
        plan: &DispatchPlan,
        state: &mut AttemptState,
    ) -> LoopStep {
        if self.config.soft_http_errors {
            if matches!(err, CrawlError::NotFound { .. }) {
                return LoopStep::Done(Ok((Self::synthesise_status(404), false)));
            }
            if matches!(err, CrawlError::Forbidden { .. } | CrawlError::WafBlocked { .. }) {
                return LoopStep::Done(Ok((Self::synthesise_status(403), false)));
            }
        }

        state.last_err = Some(err.clone());

        // ~keep Error-arm WAF classification has vendor attribution but no classifier fingerprint.
        // ~keep Use an empty fingerprint label instead of a sentinel to avoid extra Prometheus cardinality.
        let waf_signal = match &err {
            CrawlError::WafBlocked { vendor, .. } => Some(WafSignal {
                vendor: vendor.clone(),
                fingerprint_id: String::new(),
                weight: 1.0,
            }),
            _ => None,
        };

        let outcome = AttemptOutcome {
            attempt: state.attempt,
            url: std::sync::Arc::from(url),
            status: None,
            error: Some(err.clone()),
            waf_signal,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: state.current_tier,
        };
        match plan.retry_policy.decide(&outcome).await {
            RetryDirective::Stop => {
                state.report_dispatch(url, plan);
                LoopStep::Done(Err(err))
            }
            RetryDirective::Retry { backoff_ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                state.attempt += 1;
                LoopStep::Restart
            }
            RetryDirective::Escalate { reason } => {
                if let Some(next) = state.affordable_next_tier(plan).await {
                    Self::record_escalation(state.current_tier, next, &reason);
                    state.escalate_to(next, &reason);
                    return LoopStep::Restart;
                }
                state.report_dispatch(url, plan);
                LoopStep::Done(Err(err))
            }
        }
    }

    /// Count one tier transition on `backend_escalations_total`.
    fn record_escalation(from_tier: Tier, to_tier: Tier, reason: &EscalationReason) {
        crate::telemetry::metrics::registry().backend_escalations_total.add(
            1,
            &[
                KeyValue::new("from_tier", Self::tier_name(from_tier)),
                KeyValue::new("to_tier", Self::tier_name(to_tier)),
                KeyValue::new("reason", escalation_reason_label(reason)),
            ],
        );
    }
}
