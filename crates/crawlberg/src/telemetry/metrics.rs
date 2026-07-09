//! Global metric registry for crawlberg.
//!
//! [`MetricRegistry`] is a `'static` singleton obtained via [`registry()`].
//! All `crawl_*` OTel instrument handles live here; names mirror what
//! xberg-enterprise already emits so cloud can delete its bridging code.
//!
//! # Handles
//!
//! | Field | Instrument | Name |
//! |---|---|---|
//! | [`pages_total`] | `u64_counter` | `crawl_pages_total` |
//! | [`documents_discovered_total`] | `u64_counter` | `crawl_documents_discovered_total` |
//! | [`robots_blocked_total`] | `u64_counter` | `crawl_robots_blocked_total` |
//! | [`waf_blocks_total`] | `u64_counter` | `crawl_waf_blocks_total` |
//! | [`backend_escalations_total`] | `u64_counter` | `crawl_backend_escalations_total` |
//! | [`bypass_requests_total`] | `u64_counter` | `crawl_bypass_requests_total` |
//! | [`bypass_failures_total`] | `u64_counter` | `crawl_bypass_failures_total` |
//! | [`duration_seconds`] | `f64_histogram` | `crawl_duration_seconds` |
//! | [`pages_duration_seconds`] | `f64_histogram` | `crawl_pages_duration_seconds` |
//! | [`browser_sessions_active`] | `i64_up_down_counter` | `crawl_browser_sessions_active` |
//!
//! [`pages_total`]: MetricRegistry::pages_total
//! [`documents_discovered_total`]: MetricRegistry::documents_discovered_total
//! [`robots_blocked_total`]: MetricRegistry::robots_blocked_total
//! [`waf_blocks_total`]: MetricRegistry::waf_blocks_total
//! [`backend_escalations_total`]: MetricRegistry::backend_escalations_total
//! [`bypass_requests_total`]: MetricRegistry::bypass_requests_total
//! [`bypass_failures_total`]: MetricRegistry::bypass_failures_total
//! [`duration_seconds`]: MetricRegistry::duration_seconds
//! [`pages_duration_seconds`]: MetricRegistry::pages_duration_seconds
//! [`browser_sessions_active`]: MetricRegistry::browser_sessions_active

use std::sync::OnceLock;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};

/// Holds all OTel instrument handles for crawlberg.
///
/// Obtain the global instance via [`registry()`].
pub struct MetricRegistry {
    /// Pages fetched by the crawl engine, partitioned by terminal status.
    ///
    /// Labels: `status` ∈ `ok | http_error | timeout | blocked`
    pub pages_total: Counter<u64>,

    /// Documents (PDF, DOCX, …) discovered during crawling, partitioned by MIME type.
    ///
    /// Labels: `mime_type`
    pub documents_discovered_total: Counter<u64>,

    /// Page fetches blocked by robots.txt.
    pub robots_blocked_total: Counter<u64>,

    /// WAF / antibot challenges detected, partitioned by vendor.
    ///
    /// Labels: `vendor`
    pub waf_blocks_total: Counter<u64>,

    /// Tier escalations in the dispatch chain.
    ///
    /// Labels: `from_tier`, `to_tier`, `reason`
    pub backend_escalations_total: Counter<u64>,

    /// Requests routed through a bypass provider.
    ///
    /// Labels: `vendor`, `mode` ∈ `managed | byo`
    pub bypass_requests_total: Counter<u64>,

    /// Bypass provider failures, partitioned by reason.
    ///
    /// Labels: `vendor`, `reason`
    pub bypass_failures_total: Counter<u64>,

    /// End-to-end crawl duration in seconds.
    ///
    /// Labels: `output_mode`, `status`
    pub duration_seconds: Histogram<f64>,

    /// Per-page fetch duration in seconds.
    ///
    /// Labels: `host`
    pub pages_duration_seconds: Histogram<f64>,

    /// Active headless-browser sessions.
    pub browser_sessions_active: UpDownCounter<i64>,
}

static REGISTRY: OnceLock<MetricRegistry> = OnceLock::new();

impl MetricRegistry {
    fn new() -> Self {
        let meter = global::meter("crawlberg");

        let pages_total = meter
            .u64_counter("crawl_pages_total")
            .with_description("Pages fetched by the crawl engine, partitioned by terminal status")
            .build();

        let documents_discovered_total = meter
            .u64_counter("crawl_documents_discovered_total")
            .with_description("Documents (PDF, DOCX, \u{2026}) discovered during crawling, partitioned by mime type")
            .build();

        let robots_blocked_total = meter
            .u64_counter("crawl_robots_blocked_total")
            .with_description("Page fetches blocked by robots.txt")
            .build();

        let waf_blocks_total = meter
            .u64_counter("crawl_waf_blocks_total")
            .with_description("WAF / antibot challenges detected, partitioned by vendor")
            .build();

        let backend_escalations_total = meter
            .u64_counter("crawl_backend_escalations_total")
            .with_description("Tier escalations in the dispatch chain")
            .build();

        let bypass_requests_total = meter
            .u64_counter("crawl_bypass_requests_total")
            .with_description("Requests routed through a bypass provider")
            .build();

        let bypass_failures_total = meter
            .u64_counter("crawl_bypass_failures_total")
            .with_description("Bypass provider failures, partitioned by reason")
            .build();

        let duration_seconds = meter
            .f64_histogram("crawl_duration_seconds")
            .with_description("End-to-end crawl duration in seconds")
            .with_unit("s")
            .build();

        let pages_duration_seconds = meter
            .f64_histogram("crawl_pages_duration_seconds")
            .with_description("Per-page fetch duration in seconds")
            .with_unit("s")
            .build();

        let browser_sessions_active = meter
            .i64_up_down_counter("crawl_browser_sessions_active")
            .with_description("Active headless-browser sessions")
            .build();

        Self {
            pages_total,
            documents_discovered_total,
            robots_blocked_total,
            waf_blocks_total,
            backend_escalations_total,
            bypass_requests_total,
            bypass_failures_total,
            duration_seconds,
            pages_duration_seconds,
            browser_sessions_active,
        }
    }
}

/// Return a reference to the process-wide [`MetricRegistry`].
///
/// Initialises the registry on first call (lazy, thread-safe).
pub fn registry() -> &'static MetricRegistry {
    REGISTRY.get_or_init(MetricRegistry::new)
}
