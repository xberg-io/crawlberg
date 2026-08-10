//! Public types used across the crawlberg crate.

pub mod antibot;
mod builder;
mod bypass;
mod config;
mod discovery;
mod dispatch;
mod metadata;
mod results;
#[cfg(not(target_arch = "wasm32"))]
mod streaming;

#[cfg(not(target_arch = "wasm32"))]
pub use antibot::{AntibotStrategy, Decision};
pub use builder::{CrawlConfigBuilder, DispatchProfileBuilder};
pub use bypass::{BypassProvider, BypassResponse, DynBypassProvider};
pub use config::{
    AuthConfig, BrowserBackend, BrowserConfig, BrowserMode, BrowserWait, ContentConfig, CrawlConfig,
    DocumentContentEncoding, ExtractionMeta, ProxyConfig,
};
#[cfg(not(target_arch = "wasm32"))]
pub use discovery::CrawlEvent;
pub use discovery::{
    AssetCategory, CookieInfo, DownloadedAsset, FeedInfo, FeedType, ImageInfo, ImageSource, JsonLdEntry, LinkInfo,
    LinkType,
};
pub use dispatch::{
    AttemptOutcome, BudgetExhausted, DispatchProfile, DomainObservation, DomainRecommendation, DomainStatePort,
    DynDomainStatePort, DynEscalationBudget, DynRetryPolicy, DynWafClassifier, EscalationBudget, EscalationReason,
    EscalationStrategy, ObservedOutcome, RetryDirective, RetryPolicy, Tier, WafClassifier, WafClassifyError, WafSignal,
};
pub use metadata::{ArticleMetadata, FaviconInfo, HeadingInfo, HreflangEntry, PageMetadata, ResponseMeta};
pub use results::{
    ActionResult, BrowserExtras, CachedPage, CrawlPageResult, CrawlResult, DownloadedDocument, InteractionResult,
    MapResult, MarkdownResult, ScrapeResult, SitemapUrl,
};
#[cfg(not(target_arch = "wasm32"))]
pub use streaming::{BatchCrawlStreamRequest, CrawlStreamRequest};
