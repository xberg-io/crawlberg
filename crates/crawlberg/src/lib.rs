//! crawlberg -- A Rust crawling engine for turning websites into structured data.

#[cfg(feature = "api")]
pub(crate) mod api;
mod assets;
pub(crate) mod bindings;
#[cfg(feature = "browser")]
mod browser;
mod browser_detect;
// ~keep Gated on `browser-chromiumoxide`, not `browser`: `browser` implies it, and the
// ~keep interact launcher is gated on the narrower feature, so both callers can reach this.
#[cfg(feature = "browser-chromiumoxide")]
pub mod browser_pool;
#[cfg(feature = "browser")]
pub mod browser_profile;
#[cfg(feature = "browser")]
pub mod browser_session_pool;
pub mod budget;
#[cfg(feature = "browser-chromiumoxide")]
mod chrome_args;
pub(crate) mod citations;
// ~keep Gated on `browser-chromiumoxide`, not `browser`: see the module's own `~keep` header
// ~keep for why nesting it under `browser` would break a `browser-chromiumoxide`-only build
// ~keep (xberg-io/crawlberg#74).
#[cfg(feature = "browser-chromiumoxide")]
mod ssrf_intercept;
// ~keep Gated on `browser-chromiumoxide`, not `browser`, to match its callers: the interact
// ~keep launcher (interact/chromiumoxide.rs) is gated on the narrower feature and calls
// ~keep apply_stealth_patches, so a `browser-chromiumoxide`-only build compiled this module out
// ~keep from under a live call site and failed to build. No CI job exercised that feature set.
#[cfg(feature = "browser-chromiumoxide")]
mod stealth;

pub(crate) mod defaults;
mod document;
pub(crate) mod engine;
mod error;
mod helpers;
mod html;
pub mod http;
pub mod interact;
mod map;
mod markdown;
#[cfg(feature = "mcp")]
pub(crate) mod mcp;
#[cfg(feature = "browser-native")]
mod native_browser;
pub mod net;
mod normalize;
pub mod proxy;
mod pruning;
#[cfg(feature = "ai")]
pub(crate) mod research;
pub mod robots;
mod scrape;
#[cfg(not(target_arch = "wasm32"))]
pub mod sink;
pub mod sitemap;
pub mod telemetry;
pub(crate) mod time;
pub(crate) mod tower;
pub mod traits;
mod types;
pub(crate) mod waf;
#[cfg(feature = "warc")]
pub(crate) mod warc;

#[cfg(feature = "api")]
pub use api::serve_with_config as serve_api;
pub use bindings::{
    BatchCrawlResult, BatchCrawlResults, BatchScrapeResult, BatchScrapeResults, CrawlEngineHandle, batch_crawl,
    batch_scrape, crawl, create_engine, interact, map_urls, scrape,
};
#[cfg(not(target_arch = "wasm32"))]
pub use bindings::{batch_crawl_stream, crawl_stream};
#[cfg(feature = "browser")]
pub use browser_pool::{BrowserPool, BrowserPoolConfig};
#[cfg(feature = "browser")]
pub use browser_profile::BrowserProfile;
#[cfg(feature = "browser")]
pub use browser_session_pool::{BrowserSessionPool, SessionKey};
pub use budget::{BudgetError, DefaultPageBudget, PageBudget};
pub use citations::{CitationReference, CitationResult, generate_citations};
#[cfg(feature = "browser-native")]
pub use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeBrowserExecutorConfig};
#[doc(hidden)]
pub use defaults::compute_backoff_ms;
pub use defaults::{
    AdaptiveStrategy, BestFirstStrategy, BfsStrategy, Bm25Filter, DfsStrategy, EwmaDomainState, EwmaTracker,
    FixedBudget, InMemoryFrontier, LearningRetryPolicy, LifoFrontier, NoopCache, NoopEmitter, NoopFilter, NoopStore,
    PerDomainThrottle, SimpleRetryPolicy, UnlimitedBudget, default_retry_policy, in_memory_domain_state,
    unlimited_budget,
};
#[cfg(feature = "ai")]
pub use defaults::{InFlightBound, LlmExtractor, LlmExtractorConfig, LlmResponseCacheConfig};
pub use engine::{CrawlEngine, CrawlEngineBuilder};
pub use error::CrawlError;
pub use interact::{
    MAX_ACTIONS, MAX_SCRIPT_LEN, MAX_SCROLL_AMOUNT, MAX_SELECTOR_LEN, MAX_SINGLE_WAIT_MS, MAX_TEXT_LEN,
    MAX_TOTAL_WAIT_SECS, PageAction, ScrollDirection, validate_actions,
};
#[cfg(feature = "mcp-http")]
pub use mcp::start_mcp_http_server;
#[cfg(feature = "mcp")]
pub use mcp::{CrawlbergHttpMcpService, start_mcp_server, start_mcp_server_with_config, streamable_http_service};
pub use net::ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};
pub use proxy::{ProxyProvider, StaticProxyProvider};
#[cfg(not(target_arch = "wasm32"))]
pub use sink::{EventSink, MultiEventSink, TracingEventSink};
pub use telemetry::{current_traceparent, with_traceparent};
pub use types::antibot::{AntibotError, AntibotStrategy, Decision, DefaultAntibotStrategy, DynAntibotStrategy};
pub use types::{
    ActionResult, ArticleMetadata, AssetCategory, AttemptOutcome, AuthConfig, BrowserBackend, BrowserConfig,
    BrowserExtras, BrowserMode, BrowserWait, BudgetExhausted, BypassProvider, BypassResponse, CachedPage,
    ContentConfig, ContentFilterKind, CookieInfo, CrawlConfig, CrawlConfigBuilder, CrawlPageResult, CrawlResult,
    CrawlStrategyKind, DispatchProfile, DispatchProfileBuilder, DocumentContentEncoding, DomainObservation,
    DomainRecommendation, DomainStatePort, DownloadedAsset, DownloadedDocument, DynBypassProvider, DynDomainStatePort,
    DynEscalationBudget, DynRetryPolicy, DynWafClassifier, EscalationBudget, EscalationReason, EscalationStrategy,
    ExtractionMeta, FaviconInfo, FeedInfo, FeedType, HeadingInfo, HreflangEntry, ImageInfo, ImageSource,
    InteractionResult, JsonLdEntry, LinkInfo, LinkType, MapResult, MarkdownResult, ObservedOutcome, PageMetadata,
    ProxyConfig, ResponseMeta, RetryDirective, RetryPolicy, ScrapeResult, SitemapUrl, Tier, WafClassifier,
    WafClassifyError, WafSignal,
};
#[cfg(not(target_arch = "wasm32"))]
pub use types::{BatchCrawlStreamRequest, CrawlEvent, CrawlStreamRequest};
pub use waf::rules::load_from_path as waf_rules_from_path;
pub use waf::{
    Rules as WafRules, RulesError as WafRulesError, TomlClassifier, WatchError as WafWatchError,
    WatchHandle as WafWatchHandle, load_from_str as waf_rules_from_str,
};
