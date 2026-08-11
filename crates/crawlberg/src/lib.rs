//! crawlberg -- A Rust crawling engine for turning websites into structured data.

#[cfg(feature = "api")]
pub(crate) mod api;
mod assets;
pub(crate) mod bindings;
#[cfg(feature = "browser")]
mod browser;
mod browser_detect;
#[cfg(feature = "browser")]
pub mod browser_pool;
#[cfg(feature = "browser")]
pub mod browser_profile;
#[cfg(feature = "browser")]
pub mod browser_session_pool;
pub mod budget;
pub(crate) mod citations;
#[cfg(feature = "browser")]
mod stealth;

pub(crate) mod defaults;
mod document;
pub(crate) mod engine;
mod error;
#[cfg(not(target_arch = "wasm32"))]
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
    AdaptiveStrategy, BestFirstStrategy, BfsStrategy, DfsStrategy, EwmaDomainState, EwmaTracker, FixedBudget,
    InMemoryFrontier, LearningRetryPolicy, NoopCache, NoopEmitter, NoopFilter, NoopStore, PerDomainThrottle,
    SimpleRetryPolicy, UnlimitedBudget, default_retry_policy, in_memory_domain_state, unlimited_budget,
};
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
    ContentConfig, CookieInfo, CrawlConfig, CrawlConfigBuilder, CrawlPageResult, CrawlResult, DispatchProfile,
    DispatchProfileBuilder, DocumentContentEncoding, DomainObservation, DomainRecommendation, DomainStatePort,
    DownloadedAsset, DownloadedDocument, DynBypassProvider, DynDomainStatePort, DynEscalationBudget, DynRetryPolicy,
    DynWafClassifier, EscalationBudget, EscalationReason, EscalationStrategy, ExtractionMeta, FaviconInfo, FeedInfo,
    FeedType, HeadingInfo, HreflangEntry, ImageInfo, ImageSource, InteractionResult, JsonLdEntry, LinkInfo, LinkType,
    MapResult, MarkdownResult, ObservedOutcome, PageMetadata, ProxyConfig, ResponseMeta, RetryDirective, RetryPolicy,
    ScrapeResult, SitemapUrl, Tier, WafClassifier, WafClassifyError, WafSignal,
};
#[cfg(not(target_arch = "wasm32"))]
pub use types::{BatchCrawlStreamRequest, CrawlEvent, CrawlStreamRequest};
pub use waf::rules::load_from_path as waf_rules_from_path;
pub use waf::{
    Rules as WafRules, RulesError as WafRulesError, TomlClassifier, WatchError as WafWatchError,
    WatchHandle as WafWatchHandle, load_from_str as waf_rules_from_str,
};
