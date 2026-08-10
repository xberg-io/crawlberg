//! Fluent builders for [`CrawlConfig`] and [`DispatchProfile`].
//!
//! Use [`CrawlConfig::builder`] and [`DispatchProfile::builder`] as entry
//! points rather than constructing these types directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::antibot::DynAntibotStrategy;
use super::bypass::DynBypassProvider;
use super::config::{AuthConfig, BrowserConfig, ContentConfig, CrawlConfig, ProxyConfig};
use super::discovery::AssetCategory;
use super::dispatch::{
    DispatchProfile, DynDomainStatePort, DynEscalationBudget, DynRetryPolicy, DynWafClassifier, EscalationStrategy,
};
use crate::net::HostMatcher;

/// Fluent builder for [`CrawlConfig`].
///
/// Call [`CrawlConfig::builder`] to obtain an instance. All setters consume
/// `self` and return `Self`, enabling method chaining. Unset fields fall back
/// to [`CrawlConfig::default`].
///
/// # Example
///
/// ```
/// use crawlberg::{CrawlConfig, DispatchProfile, EscalationStrategy};
///
/// let config = CrawlConfig::builder()
///     .max_pages(50)
///     .stay_on_domain(true)
///     .dispatch(
///         DispatchProfile::builder()
///             .strategy(EscalationStrategy::BrowserOnly)
///             .build(),
///     )
///     .build();
///
/// assert_eq!(config.max_pages, Some(50));
/// assert!(config.stay_on_domain);
/// ```
#[derive(Default)]
pub struct CrawlConfigBuilder {
    inner: CrawlConfig,
}

impl CrawlConfigBuilder {
    /// Set the maximum crawl depth.
    pub fn max_depth(mut self, value: usize) -> Self {
        self.inner.max_depth = Some(value);
        self
    }

    /// Set the maximum number of pages to crawl.
    pub fn max_pages(mut self, value: usize) -> Self {
        self.inner.max_pages = Some(value);
        self
    }

    /// Set the maximum number of links enqueued from a single page.
    pub fn max_links_per_page(mut self, value: usize) -> Self {
        self.inner.max_links_per_page = Some(value);
        self
    }

    /// Set the maximum number of concurrent requests.
    pub fn max_concurrent(mut self, value: usize) -> Self {
        self.inner.max_concurrent = Some(value);
        self
    }

    /// Set whether to respect robots.txt directives.
    pub fn respect_robots_txt(mut self, value: bool) -> Self {
        self.inner.respect_robots_txt = value;
        self
    }

    /// Set whether HTTP-level errors are surfaced as soft results.
    pub fn soft_http_errors(mut self, value: bool) -> Self {
        self.inner.soft_http_errors = value;
        self
    }

    /// Set a custom user-agent string.
    pub fn user_agent(mut self, value: impl Into<String>) -> Self {
        self.inner.user_agent = Some(value.into());
        self
    }

    /// Set whether crawling is restricted to the starting domain.
    pub fn stay_on_domain(mut self, value: bool) -> Self {
        self.inner.stay_on_domain = value;
        self
    }

    /// Set whether subdomains are allowed when `stay_on_domain` is true.
    pub fn allow_subdomains(mut self, value: bool) -> Self {
        self.inner.allow_subdomains = value;
        self
    }

    /// Set regex patterns for paths to include during crawling.
    pub fn include_paths(mut self, value: Vec<String>) -> Self {
        self.inner.include_paths = value;
        self
    }

    /// Set regex patterns for paths to exclude during crawling.
    pub fn exclude_paths(mut self, value: Vec<String>) -> Self {
        self.inner.exclude_paths = value;
        self
    }

    /// Set custom HTTP headers to send with each request.
    pub fn custom_headers(mut self, value: HashMap<String, String>) -> Self {
        self.inner.custom_headers = value;
        self
    }

    /// Set the timeout for individual HTTP requests.
    pub fn request_timeout(mut self, value: Duration) -> Self {
        self.inner.request_timeout = value;
        self
    }

    /// Set the per-domain rate limit in milliseconds.
    pub fn rate_limit_ms(mut self, value: u64) -> Self {
        self.inner.rate_limit_ms = Some(value);
        self
    }

    /// Set the maximum number of redirects to follow.
    pub fn max_redirects(mut self, value: usize) -> Self {
        self.inner.max_redirects = value;
        self
    }

    /// Set the number of retry attempts for failed requests.
    pub fn retry_count(mut self, value: usize) -> Self {
        self.inner.retry_count = value;
        self
    }

    /// Set the HTTP status codes that trigger a retry.
    pub fn retry_codes(mut self, value: Vec<u16>) -> Self {
        self.inner.retry_codes = value;
        self
    }

    /// Set whether cookie handling is enabled.
    pub fn cookies_enabled(mut self, value: bool) -> Self {
        self.inner.cookies_enabled = value;
        self
    }

    /// Set the authentication configuration.
    pub fn auth(mut self, value: AuthConfig) -> Self {
        self.inner.auth = Some(value);
        self
    }

    /// Set the maximum response body size in bytes.
    pub fn max_body_size(mut self, value: usize) -> Self {
        self.inner.max_body_size = Some(value);
        self
    }

    /// Set CSS selectors for tags to remove from HTML before processing.
    pub fn remove_tags(mut self, value: Vec<String>) -> Self {
        self.inner.remove_tags = value;
        self
    }

    /// Set the content extraction and conversion configuration.
    pub fn content(mut self, value: ContentConfig) -> Self {
        self.inner.content = value;
        self
    }

    /// Set the maximum number of URLs to return from a map operation.
    pub fn map_limit(mut self, value: usize) -> Self {
        self.inner.map_limit = Some(value);
        self
    }

    /// Set the search filter for map results.
    pub fn map_search(mut self, value: impl Into<String>) -> Self {
        self.inner.map_search = Some(value.into());
        self
    }

    /// Set whether to download assets from the page.
    pub fn download_assets(mut self, value: bool) -> Self {
        self.inner.download_assets = value;
        self
    }

    /// Set the filter for asset categories to download.
    pub fn asset_types(mut self, value: Vec<AssetCategory>) -> Self {
        self.inner.asset_types = value;
        self
    }

    /// Set the maximum size in bytes for individual asset downloads.
    pub fn max_asset_size(mut self, value: usize) -> Self {
        self.inner.max_asset_size = Some(value);
        self
    }

    /// Set the browser configuration.
    pub fn browser(mut self, value: BrowserConfig) -> Self {
        self.inner.browser = value;
        self
    }

    /// Set the proxy configuration for HTTP requests.
    pub fn proxy(mut self, value: ProxyConfig) -> Self {
        self.inner.proxy = Some(value);
        self
    }

    /// Set the list of user-agent strings for rotation.
    pub fn user_agents(mut self, value: Vec<String>) -> Self {
        self.inner.user_agents = value;
        self
    }

    /// Set whether to capture a screenshot when using the browser.
    pub fn capture_screenshot(mut self, value: bool) -> Self {
        self.inner.capture_screenshot = value;
        self
    }

    /// Set whether to download non-HTML documents.
    pub fn download_documents(mut self, value: bool) -> Self {
        self.inner.download_documents = value;
        self
    }

    /// Set the maximum size in bytes for document downloads.
    pub fn document_max_size(mut self, value: usize) -> Self {
        self.inner.document_max_size = Some(value);
        self
    }

    /// Set the allowlist of MIME types to download.
    pub fn document_mime_types(mut self, value: Vec<String>) -> Self {
        self.inner.document_mime_types = value;
        self
    }

    /// Set the path to write WARC output.
    pub fn warc_output(mut self, value: PathBuf) -> Self {
        self.inner.warc_output = Some(value);
        self
    }

    /// Set the named browser profile for persistent sessions.
    pub fn browser_profile(mut self, value: impl Into<String>) -> Self {
        self.inner.browser_profile = Some(value.into());
        self
    }

    /// Set whether to save changes back to the browser profile on exit.
    pub fn save_browser_profile(mut self, value: bool) -> Self {
        self.inner.save_browser_profile = value;
        self
    }

    /// Configure whether to allow private network IP ranges in SSRF policy.
    ///
    /// When `true`, private IPs (loopback, 10/8, 192.168/16, etc.) are allowed.
    /// When `false` (default), they are denied unless explicitly allowlisted.
    pub fn allow_private_networks(mut self, allow: bool) -> Self {
        self.inner.ssrf.deny_private = !allow;
        self
    }

    /// Add a hostname or IP range to the SSRF policy allowlist.
    ///
    /// Entries override `deny_private` for whatever they match; see
    /// [`SsrfPolicy::allowlist`](crate::SsrfPolicy::allowlist) for the full precedence.
    ///
    /// ```
    /// use crawlberg::{CrawlConfigBuilder, HostMatcher};
    ///
    /// let config = CrawlConfigBuilder::default()
    ///     .ssrf_allowlist_host(HostMatcher::suffix(".internal.example.com"))
    ///     .ssrf_allowlist_host(HostMatcher::cidr("10.42.0.0/16")?)
    ///     .build();
    /// # Ok::<(), crawlberg::SsrfError>(())
    /// ```
    pub fn ssrf_allowlist_host(mut self, matcher: HostMatcher) -> Self {
        self.inner.ssrf.allowlist.push(matcher);
        self
    }

    /// Set the pluggable dispatch profile.
    pub fn dispatch(mut self, value: DispatchProfile) -> Self {
        self.inner.dispatch = Some(value);
        self
    }

    /// Consume the builder and produce a [`CrawlConfig`].
    pub fn build(self) -> CrawlConfig {
        self.inner
    }
}

/// Fluent builder for [`DispatchProfile`].
///
/// Call [`DispatchProfile::builder`] to obtain an instance. All setters
/// consume `self` and return `Self`, enabling method chaining. Unset fields
/// fall back to [`DispatchProfile::default`].
///
/// # Example
///
/// ```
/// use crawlberg::{DispatchProfile, EscalationStrategy};
///
/// let profile = DispatchProfile::builder()
///     .strategy(EscalationStrategy::BypassThenBrowser)
///     .max_total_attempts(5)
///     .build();
///
/// assert_eq!(profile.strategy, EscalationStrategy::BypassThenBrowser);
/// assert_eq!(profile.max_total_attempts, 5);
/// ```
#[derive(Default)]
pub struct DispatchProfileBuilder {
    inner: DispatchProfile,
}

impl DispatchProfileBuilder {
    /// Set the caller-supplied bypass provider.
    pub fn bypass(mut self, provider: DynBypassProvider) -> Self {
        self.inner.bypass = Some(provider);
        self
    }

    /// Set the escalation strategy for the HTTP → Bypass → Browser chain.
    pub fn strategy(mut self, value: EscalationStrategy) -> Self {
        self.inner.strategy = value;
        self
    }

    /// Set the pluggable per-attempt retry/escalation decision policy.
    pub fn retry_policy(mut self, policy: DynRetryPolicy) -> Self {
        self.inner.retry_policy = Some(policy);
        self
    }

    /// Set the pluggable WAF classifier.
    pub fn waf_classifier(mut self, classifier: DynWafClassifier) -> Self {
        self.inner.waf_classifier = Some(classifier);
        self
    }

    /// Set the pluggable per-domain state backend.
    pub fn domain_state(mut self, state: DynDomainStatePort) -> Self {
        self.inner.domain_state = Some(state);
        self
    }

    /// Set the pluggable per-job escalation budget.
    pub fn escalation_budget(mut self, budget: DynEscalationBudget) -> Self {
        self.inner.escalation_budget = Some(budget);
        self
    }

    /// Set the maximum total fetch attempts across all tiers.
    pub fn max_total_attempts(mut self, value: u32) -> Self {
        self.inner.max_total_attempts = value;
        self
    }

    /// Set the pluggable antibot hook pair.
    ///
    /// When set, `pre_request` fires before each fetch and `post_response`
    /// fires between WAF classification and the retry policy.
    pub fn antibot_strategy(mut self, strategy: DynAntibotStrategy) -> Self {
        self.inner.antibot_strategy = Some(strategy);
        self
    }

    /// Consume the builder and produce a [`DispatchProfile`].
    pub fn build(self) -> DispatchProfile {
        self.inner
    }
}
