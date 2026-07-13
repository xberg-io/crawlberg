require "json"

# Low-level binding to the generated C FFI layer (crawlberg.h).
#
# Every non-scalar value crosses the C ABI as a NUL-terminated JSON string
# (`LibC::Char*`); scalars pass by value. Strings returned by the library are
# owned by Rust and must be released with `cberg_free_string`.
#
# Link against the FFI shared library. The library must be installed to a
# standard path, or you can pass --link-flags at build time:
#   crystal build ... --link-flags="-L/path/to/lib -Wl,-rpath,/path/to/lib"
@[Link(ldflags: "-lcrawlberg_ffi")]
lib LibCberg
  fun free_string = cberg_free_string(ptr : LibC::Char*) : Void
  fun last_error_code = cberg_last_error_code() : Int32
  fun last_error_context = cberg_last_error_context() : LibC::Char*

  struct BatchCrawlResults
    _data : Void*
  end
  struct BatchCrawlStreamRequest
    _data : Void*
  end
  struct BatchScrapeResults
    _data : Void*
  end
  struct BrowserConfig
    _data : Void*
  end
  struct CitationResult
    _data : Void*
  end
  struct ContentConfig
    _data : Void*
  end
  struct CrawlConfig
    _data : Void*
  end
  struct CrawlResult
    _data : Void*
  end
  struct CrawlStreamRequest
    _data : Void*
  end
  struct InteractionResult
    _data : Void*
  end
  struct MapResult
    _data : Void*
  end
  struct PageAction
    _data : Void*
  end
  struct ScrapeResult
    _data : Void*
  end
  struct SsrfPolicy
    _data : Void*
  end
  fun batch_crawl_results_from_json = cberg_batch_crawl_results_from_json(json : LibC::Char*) : BatchCrawlResults*
  fun batch_crawl_results_to_json = cberg_batch_crawl_results_to_json(ptr : BatchCrawlResults*) : LibC::Char*
  fun batch_crawl_results_free = cberg_batch_crawl_results_free(ptr : BatchCrawlResults*)
  fun batch_crawl_stream_request_from_json = cberg_batch_crawl_stream_request_from_json(json : LibC::Char*) : BatchCrawlStreamRequest*
  fun batch_crawl_stream_request_to_json = cberg_batch_crawl_stream_request_to_json(ptr : BatchCrawlStreamRequest*) : LibC::Char*
  fun batch_crawl_stream_request_free = cberg_batch_crawl_stream_request_free(ptr : BatchCrawlStreamRequest*)
  fun batch_scrape_results_from_json = cberg_batch_scrape_results_from_json(json : LibC::Char*) : BatchScrapeResults*
  fun batch_scrape_results_to_json = cberg_batch_scrape_results_to_json(ptr : BatchScrapeResults*) : LibC::Char*
  fun batch_scrape_results_free = cberg_batch_scrape_results_free(ptr : BatchScrapeResults*)
  fun browser_config_from_json = cberg_browser_config_from_json(json : LibC::Char*) : BrowserConfig*
  fun browser_config_to_json = cberg_browser_config_to_json(ptr : BrowserConfig*) : LibC::Char*
  fun browser_config_free = cberg_browser_config_free(ptr : BrowserConfig*)
  fun citation_result_from_json = cberg_citation_result_from_json(json : LibC::Char*) : CitationResult*
  fun citation_result_to_json = cberg_citation_result_to_json(ptr : CitationResult*) : LibC::Char*
  fun citation_result_free = cberg_citation_result_free(ptr : CitationResult*)
  fun content_config_from_json = cberg_content_config_from_json(json : LibC::Char*) : ContentConfig*
  fun content_config_to_json = cberg_content_config_to_json(ptr : ContentConfig*) : LibC::Char*
  fun content_config_free = cberg_content_config_free(ptr : ContentConfig*)
  fun crawl_config_from_json = cberg_crawl_config_from_json(json : LibC::Char*) : CrawlConfig*
  fun crawl_config_to_json = cberg_crawl_config_to_json(ptr : CrawlConfig*) : LibC::Char*
  fun crawl_config_free = cberg_crawl_config_free(ptr : CrawlConfig*)
  fun crawl_result_from_json = cberg_crawl_result_from_json(json : LibC::Char*) : CrawlResult*
  fun crawl_result_to_json = cberg_crawl_result_to_json(ptr : CrawlResult*) : LibC::Char*
  fun crawl_result_free = cberg_crawl_result_free(ptr : CrawlResult*)
  fun crawl_stream_request_from_json = cberg_crawl_stream_request_from_json(json : LibC::Char*) : CrawlStreamRequest*
  fun crawl_stream_request_to_json = cberg_crawl_stream_request_to_json(ptr : CrawlStreamRequest*) : LibC::Char*
  fun crawl_stream_request_free = cberg_crawl_stream_request_free(ptr : CrawlStreamRequest*)
  fun interaction_result_from_json = cberg_interaction_result_from_json(json : LibC::Char*) : InteractionResult*
  fun interaction_result_to_json = cberg_interaction_result_to_json(ptr : InteractionResult*) : LibC::Char*
  fun interaction_result_free = cberg_interaction_result_free(ptr : InteractionResult*)
  fun map_result_from_json = cberg_map_result_from_json(json : LibC::Char*) : MapResult*
  fun map_result_to_json = cberg_map_result_to_json(ptr : MapResult*) : LibC::Char*
  fun map_result_free = cberg_map_result_free(ptr : MapResult*)
  fun page_action_from_json = cberg_page_action_from_json(json : LibC::Char*) : PageAction*
  fun page_action_to_json = cberg_page_action_to_json(ptr : PageAction*) : LibC::Char*
  fun page_action_free = cberg_page_action_free(ptr : PageAction*)
  fun scrape_result_from_json = cberg_scrape_result_from_json(json : LibC::Char*) : ScrapeResult*
  fun scrape_result_to_json = cberg_scrape_result_to_json(ptr : ScrapeResult*) : LibC::Char*
  fun scrape_result_free = cberg_scrape_result_free(ptr : ScrapeResult*)
  fun ssrf_policy_from_json = cberg_ssrf_policy_from_json(json : LibC::Char*) : SsrfPolicy*
  fun ssrf_policy_to_json = cberg_ssrf_policy_to_json(ptr : SsrfPolicy*) : LibC::Char*
  fun ssrf_policy_free = cberg_ssrf_policy_free(ptr : SsrfPolicy*)

  # Convert markdown links to numbered citations.
  fun generate_citations = cberg_generate_citations(markdown : LibC::Char*) : CitationResult*
  # Create a new crawl engine with the given configuration.
  fun create_engine = cberg_create_engine(config : CrawlConfig*) : Void*
  # Scrape a single URL, returning extracted page data.
  fun scrape = cberg_scrape(engine : Void*, url : LibC::Char*) : ScrapeResult*
  # Crawl a website starting from `url`, following links up to the configured depth.
  fun crawl = cberg_crawl(engine : Void*, url : LibC::Char*) : CrawlResult*
  # Discover all pages on a website by following links and sitemaps.
  fun map_urls = cberg_map_urls(engine : Void*, url : LibC::Char*) : MapResult*
  # Execute browser actions on a single page.
  fun interact = cberg_interact(engine : Void*, url : LibC::Char*, actions : LibC::Char*) : InteractionResult*
  # Scrape multiple URLs concurrently.
  fun batch_scrape = cberg_batch_scrape(engine : Void*, urls : LibC::Char*) : BatchScrapeResults*
  # Crawl multiple seed URLs concurrently, each following links to configured depth.
  fun batch_crawl = cberg_batch_crawl(engine : Void*, urls : LibC::Char*) : BatchCrawlResults*
  fun crawl_engine_handle_free = cberg_crawl_engine_handle_free(handle : Void*) : Void
  fun crawl_engine_handle_crawl_stream_start = cberg_crawl_engine_handle_crawl_stream_start(handle : Void*, req : CrawlStreamRequest*) : Void*
  fun crawl_engine_handle_crawl_stream_next = cberg_crawl_engine_handle_crawl_stream_next(handle : Void*) : Void*
  fun crawl_engine_handle_crawl_stream_free = cberg_crawl_engine_handle_crawl_stream_free(handle : Void*) : Void
  fun crawl_event_to_json = cberg_crawl_event_to_json(chunk : Void*) : LibC::Char*
  fun crawl_event_free = cberg_crawl_event_free(chunk : Void*) : Void
  fun crawl_engine_handle_batch_crawl_stream_start = cberg_crawl_engine_handle_batch_crawl_stream_start(handle : Void*, req : BatchCrawlStreamRequest*) : Void*
  fun crawl_engine_handle_batch_crawl_stream_next = cberg_crawl_engine_handle_batch_crawl_stream_next(handle : Void*) : Void*
  fun crawl_engine_handle_batch_crawl_stream_free = cberg_crawl_engine_handle_batch_crawl_stream_free(handle : Void*) : Void
end

# crawlberg — Crystal bindings generated by alef.
#
# Ruby-style API over the Rust core: snake_case methods, PascalCase types,
# Rust-like generic containers (`Array(T)`, `Hash(K, V)`), and fiber/`Channel`
# based concurrency for async and streaming methods.
module Crawlberg
  VERSION = "1.0.3"

  # Metadata about an LLM extraction pass.
  class ExtractionMeta
    include JSON::Serializable
    # Estimated cost of the LLM call in USD.
    getter cost : Float64?
    # Number of prompt (input) tokens consumed.
    getter prompt_tokens : UInt64?
    # Number of completion (output) tokens generated.
    getter completion_tokens : UInt64?
    # The model identifier used for extraction.
    getter model : String?
    # Number of content chunks sent to the LLM.
    getter chunks_processed : UInt64 = 0
  end

  # Proxy configuration for HTTP requests.
  class ProxyConfig
    include JSON::Serializable
    # Proxy URL (e.g. "http://proxy:8080", "socks5://proxy:1080").
    getter url : String = ""
    # Optional username for proxy authentication.
    getter username : String?
    # Optional password for proxy authentication.
    getter password : String?
  end

  # Content extraction and conversion configuration.
  #
  # Controls how HTML is converted to the output format. Uses
  # html-to-markdown-rs as the conversion engine for all formats
  # (markdown, plain text, djot).
  class ContentConfig
    include JSON::Serializable
    # Output format: `"markdown"` (default), `"plain"`, `"djot"`.
    getter output_format : String = "markdown"
    # Preprocessing aggressiveness: `"minimal"`, `"standard"` (default), `"aggressive"`.
    #
    # - Minimal: only scripts/styles removed.
    # - Standard: also removes nav, nav-hinted headers/footers/asides, forms.
    # - Aggressive: removes all footers/asides unconditionally.
    getter preprocessing_preset : String = "standard"
    # Remove navigation elements (nav, breadcrumbs, menus). Default: `true`.
    getter remove_navigation : Bool = true
    # Remove form elements. Default: `true`.
    getter remove_forms : Bool = true
    # HTML tag names to strip (render children only, remove the tag wrapper).
    # Default: `["noscript"]`.
    getter strip_tags : Array(String) = [] of String
    # HTML tag names to preserve as raw HTML in output.
    getter preserve_tags : Array(String) = [] of String
    # CSS selectors for elements to exclude entirely (element + all content).
    #
    # Unlike `strip_tags` (which removes the wrapper but keeps children),
    # excluded elements and all descendants are dropped. Supports CSS selectors:
    # `.class`, `#id`, `[attribute]`, compound selectors.
    #
    # Example: `[".cookie-banner", "#ad-container", "[role='complementary']"]`
    getter exclude_selectors : Array(String) = [] of String
    # Skip image elements in output. Default: `false`.
    getter skip_images : Bool = false
    # Max DOM traversal depth. Prevents stack overflow on deeply nested HTML.
    getter max_depth : UInt64?
    # Enable line wrapping. Default: `false`.
    getter wrap : Bool = false
    # Wrap width when `wrap` is enabled. Default: `80`.
    getter wrap_width : UInt64 = 80
    # Include document structure tree in output. Default: `true`.
    getter include_document_structure : Bool = true
  end

  # Browser fallback configuration.
  class BrowserConfig
    include JSON::Serializable
    # When to use the headless browser fallback.
    getter mode : BrowserMode = BrowserMode::Auto
    # Browser backend used to render JavaScript-heavy pages.
    getter backend : BrowserBackend = BrowserBackend::Chromiumoxide
    # CDP WebSocket endpoint for connecting to an external browser instance.
    getter endpoint : String?
    # Timeout for browser page load and rendering (in milliseconds when serialized).
    getter timeout : Int64 = 30000
    # Wait strategy after browser navigation.
    getter wait : BrowserWait = BrowserWait::NetworkIdle
    # CSS selector to wait for when `wait` is `Selector`.
    getter wait_selector : String?
    # Extra time to wait after the wait condition is met.
    getter extra_wait : Int64?
    # Proxy for browser fetches. Overrides `CrawlConfig.proxy` when set.
    # Native backend supports http/https only (no SOCKS5).
    getter proxy : ProxyConfig?
    # URL patterns to block before the network request fires. Supports `*`
    # wildcards. Useful for skipping ads/analytics/large images. Honored by
    # `BrowserBackend::Native`; chromiumoxide ignores this field today.
    getter block_url_patterns : Array(String) = [] of String
    # JavaScript snippet evaluated after navigation completes.
    #
    # Scraping captures the native backend result in `ScrapeResult.browser.eval_result`.
    # Interactions run this script before page actions on both browser backends but do
    # not include the script result in `InteractionResult`.
    getter eval_script : String?
    # User-agent used when fetching robots.txt. Defaults to `BrowserConfig.user_agent`
    # (or crawlberg's default) if unset. Native only.
    getter robots_user_agent : String?
    # Capture the full network event stream into the result. Default false
    # (only the document event is captured). Native only.
    getter capture_network_events : Bool = false
    # Enable session affinity: reuse chromiumoxide Pages for same-domain
    # requests so cookies + fingerprint + solved challenges persist.
    # Default: true. When false, each request gets a fresh Page.
    getter session_affinity : Bool = true
  end

  # Configuration for crawl, scrape, and map operations.
  class CrawlConfig
    include JSON::Serializable
    # Maximum crawl depth (number of link hops from the start URL).
    getter max_depth : UInt64?
    # Maximum number of pages to crawl.
    getter max_pages : UInt64?
    # Maximum number of concurrent requests.
    getter max_concurrent : UInt64?
    # Whether to respect robots.txt directives.
    getter respect_robots_txt : Bool = false
    # When true, HTTP-level error responses (404 NotFound, 403 Forbidden, WAF blocks)
    # are surfaced as `ScrapeResult` records with the matching `status_code` rather
    # than raised as `CrawlError`. Default `false` preserves the historical
    # throw-on-error contract for direct fetches. Independently of this flag,
    # 404s reached at the end of a redirect chain are *always* surfaced softly —
    # the user opted into redirect-following, so receiving a 404 there is part of
    # the normal flow rather than an unexpected error.
    getter soft_http_errors : Bool = false
    # Custom user-agent string.
    getter user_agent : String?
    # Whether to restrict crawling to the same domain.
    getter stay_on_domain : Bool = false
    # Whether to allow subdomains when `stay_on_domain` is true.
    getter allow_subdomains : Bool = false
    # Regex patterns for paths to include during crawling.
    getter include_paths : Array(String) = [] of String
    # Regex patterns for paths to exclude during crawling.
    getter exclude_paths : Array(String) = [] of String
    # Custom HTTP headers to send with each request.
    getter custom_headers : Hash(String, String) = {} of String => String
    # Timeout for individual HTTP requests (in milliseconds when serialized).
    getter request_timeout : Int64 = 30000
    # Per-domain rate limit in milliseconds. When set, enforces a minimum delay
    # between requests to the same domain. Defaults to 200ms when `None`.
    getter rate_limit_ms : UInt64?
    # Maximum number of redirects to follow.
    getter max_redirects : UInt64 = 10
    # Number of retry attempts for failed requests.
    getter retry_count : UInt64 = 0
    # HTTP status codes that should trigger a retry.
    getter retry_codes : Array(UInt16) = [] of UInt16
    # Whether to enable cookie handling.
    getter cookies_enabled : Bool = false
    # Authentication configuration.
    getter auth : AuthConfig?
    # Maximum response body size in bytes.
    getter max_body_size : UInt64?
    # CSS selectors for tags to remove from HTML before processing.
    getter remove_tags : Array(String) = [] of String
    # Content extraction and conversion configuration.
    getter content : ContentConfig = ContentConfig.from_json("{}")
    # Maximum number of URLs to return from a map operation.
    getter map_limit : UInt64?
    # Search filter for map results (case-insensitive substring match on URLs).
    getter map_search : String?
    # Whether to download assets (CSS, JS, images, etc.) from the page.
    getter download_assets : Bool = false
    # Filter for asset categories to download.
    getter asset_types : Array(AssetCategory) = [] of AssetCategory
    # Maximum size in bytes for individual asset downloads.
    getter max_asset_size : UInt64?
    # Browser configuration.
    getter browser : BrowserConfig = BrowserConfig.from_json("{}")
    # Proxy configuration for HTTP requests.
    getter proxy : ProxyConfig?
    # List of user-agent strings for rotation. If non-empty, overrides `user_agent`.
    getter user_agents : Array(String) = [] of String
    # Whether to capture a screenshot when using the browser.
    getter capture_screenshot : Bool = false
    # Re-enqueue discovered `LinkType::Document` URLs into the crawl frontier so
    # the crawl follows links *from* document pages (PDFs, etc.) as it would
    # from HTML pages. Default: `false` (documents terminate at materialisation).
    getter follow_document_urls : Bool = false
    # Maximum document-depth (from the seed URL through document links only)
    # when `follow_document_urls` is true. `None` means inherit `max_depth`.
    # Independent of `max_depth`: a document URL is enqueued only if BOTH the
    # outer `max_depth` and (if set) `document_url_depth` permit it.
    getter document_url_depth : UInt32?
    # Whether to download non-HTML documents (PDF, DOCX, images, code, etc.) instead of skipping them.
    getter download_documents : Bool = true
    # Maximum size in bytes for document downloads. Defaults to 50 MB.
    getter document_max_size : UInt64?
    # Allowlist of MIME types to download. If empty, uses built-in defaults.
    getter document_mime_types : Array(String) = [] of String
    # Path to write WARC output. If `None`, WARC output is disabled.
    getter warc_output : String?
    # Named browser profile for persistent sessions (cookies, localStorage).
    getter browser_profile : String?
    # Whether to save changes back to the browser profile on exit.
    getter save_browser_profile : Bool = false
    # SSRF policy for outbound network requests. Default: deny private networks,
    # allow http/https only, max 5 redirects.
    #
    # Phase 1: `deny_private` and `max_redirects` are exposed to all language
    # bindings. `allowlist` is skipped (see `SsrfPolicy` fields) and will be
    # added in a follow-up when `HostMatcher`'s tagged-enum FFI form is decided.
    getter ssrf : SsrfPolicy = SsrfPolicy.from_json("{}")
  end

  # Browser-specific extras populated when the native browser backend was used.
  #
  # Available on `ScrapeResult.browser` when `BrowserBackend::Native` handled the request.
  class BrowserExtras
    include JSON::Serializable
    # Return value of `BrowserConfig.eval_script`, if provided.
    getter eval_result : JSON::Any?
    # Network events captured during page navigation (only populated when
    # `BrowserConfig.capture_network_events` is true).
    getter network_events : Array(ResponseMeta) = [] of ResponseMeta
    # All non-expired cookies present in the browser's cookie jar after
    # navigation completes (includes both prior cookies and server Set-Cookie).
    getter cookies : Array(CookieInfo) = [] of CookieInfo
  end

  # A downloaded non-HTML document (PDF, DOCX, image, code file, etc.).
  #
  # When the crawler encounters non-HTML content and `download_documents` is
  # enabled, it downloads the raw bytes and populates this struct instead of
  # skipping the resource.
  class DownloadedDocument
    include JSON::Serializable
    # The URL the document was fetched from.
    getter url : String = ""
    # The MIME type from the Content-Type header.
    getter mime_type : String = ""
    # Size of the document in bytes.
    getter size : UInt64 = 0
    # Filename extracted from Content-Disposition or URL path.
    getter filename : String?
    # SHA-256 hex digest of the content.
    getter content_hash : String = ""
    # Selected response headers.
    getter headers : Hash(String, String) = {} of String => String
  end

  # Result of executing a sequence of page interaction actions.
  class InteractionResult
    include JSON::Serializable
    # Results from each executed action.
    getter action_results : Array(ActionResult) = [] of ActionResult
    # Final page HTML after all actions completed.
    getter final_html : String = ""
    # Final page URL (may have changed due to navigation).
    getter final_url : String = ""
  end

  # Result from a single page action execution.
  class ActionResult
    include JSON::Serializable
    # Zero-based index of the action in the sequence.
    getter action_index : UInt64 = 0
    # The type of action that was executed.
    getter action_type : String = ""
    # Whether the action completed successfully.
    getter success : Bool = false
    # Action-specific return data (screenshot bytes, JS return value, scraped HTML).
    getter data : JSON::Any?
    # Error message if the action failed.
    getter error : String?
  end

  # The result of a single-page scrape operation.
  class ScrapeResult
    include JSON::Serializable
    # The HTTP status code of the response.
    getter status_code : UInt16 = 0
    # The final URL after following all redirects.
    getter final_url : String = ""
    # The Content-Type header value.
    getter content_type : String = ""
    # The HTML body of the response.
    getter html : String = ""
    # The size of the response body in bytes.
    getter body_size : UInt64 = 0
    # Extracted metadata from the page.
    getter metadata : PageMetadata = PageMetadata.from_json("{}")
    # Links found on the page.
    getter links : Array(LinkInfo) = [] of LinkInfo
    # Images found on the page.
    getter images : Array(ImageInfo) = [] of ImageInfo
    # Feed links found on the page.
    getter feeds : Array(FeedInfo) = [] of FeedInfo
    # JSON-LD entries found on the page.
    getter json_ld : Array(JsonLdEntry) = [] of JsonLdEntry
    # Whether the URL is allowed by robots.txt.
    getter is_allowed : Bool = false
    # The crawl delay from robots.txt, in seconds.
    getter crawl_delay : UInt64?
    # Whether a noindex directive was detected.
    getter noindex_detected : Bool = false
    # Whether a nofollow directive was detected.
    getter nofollow_detected : Bool = false
    # The X-Robots-Tag header value, if present.
    getter x_robots_tag : String?
    # Whether the content is a PDF.
    getter is_pdf : Bool = false
    # Whether the page was skipped (binary or PDF content).
    getter was_skipped : Bool = false
    # The detected character set encoding.
    getter detected_charset : String?
    # Whether an authentication header was sent with the request.
    getter auth_header_sent : Bool = false
    # Response metadata extracted from HTTP headers.
    getter response_meta : ResponseMeta?
    # Downloaded assets from the page.
    getter assets : Array(DownloadedAsset) = [] of DownloadedAsset
    # Whether the page content suggests JavaScript rendering is needed.
    getter js_render_hint : Bool = false
    # Whether the browser fallback was used to fetch this page.
    getter browser_used : Bool = false
    # Markdown conversion of the page content.
    getter markdown : MarkdownResult?
    # Structured data extracted by LLM. Populated when extraction is configured.
    getter extracted_data : JSON::Any?
    # Metadata about the LLM extraction pass (cost, tokens, model).
    getter extraction_meta : ExtractionMeta?
    # Downloaded non-HTML document (PDF, DOCX, image, code, etc.).
    getter downloaded_document : DownloadedDocument?
    # Browser-specific extras (eval result, network events, cookies). Only
    # populated when `BrowserBackend::Native` was used for this request.
    getter browser : BrowserExtras?
  end

  # The result of crawling a single page during a crawl operation.
  class CrawlPageResult
    include JSON::Serializable
    # The original URL of the page.
    getter url : String = ""
    # The normalized URL of the page.
    getter normalized_url : String = ""
    # The HTTP status code of the response.
    getter status_code : UInt16 = 0
    # The Content-Type header value.
    getter content_type : String = ""
    # The HTML body of the response.
    getter html : String = ""
    # The size of the response body in bytes.
    getter body_size : UInt64 = 0
    # Extracted metadata from the page.
    getter metadata : PageMetadata = PageMetadata.from_json("{}")
    # Links found on the page.
    getter links : Array(LinkInfo) = [] of LinkInfo
    # Images found on the page.
    getter images : Array(ImageInfo) = [] of ImageInfo
    # Feed links found on the page.
    getter feeds : Array(FeedInfo) = [] of FeedInfo
    # JSON-LD entries found on the page.
    getter json_ld : Array(JsonLdEntry) = [] of JsonLdEntry
    # The depth of this page from the start URL.
    getter depth : UInt64 = 0
    # Whether this page is on the same domain as the start URL.
    getter stayed_on_domain : Bool = false
    # Whether this page was skipped (binary or PDF content).
    getter was_skipped : Bool = false
    # Whether the content is a PDF.
    getter is_pdf : Bool = false
    # The detected character set encoding.
    getter detected_charset : String?
    # Markdown conversion of the page content.
    getter markdown : MarkdownResult?
    # Structured data extracted by LLM. Populated when extraction is configured.
    getter extracted_data : JSON::Any?
    # Metadata about the LLM extraction pass (cost, tokens, model).
    getter extraction_meta : ExtractionMeta?
    # Downloaded non-HTML document (PDF, DOCX, image, code, etc.).
    getter downloaded_document : DownloadedDocument?
    # Whether the browser fallback was used to fetch this page.
    getter browser_used : Bool = false
  end

  # The result of a multi-page crawl operation.
  class CrawlResult
    include JSON::Serializable
    # The list of crawled pages.
    getter pages : Array(CrawlPageResult) = [] of CrawlPageResult
    # The final URL after following redirects.
    getter final_url : String = ""
    # The number of redirects followed.
    getter redirect_count : UInt64 = 0
    # Whether any page was skipped during crawling.
    getter was_skipped : Bool = false
    # An error message, if the crawl encountered an issue.
    getter error : String?
    # Cookies collected during the crawl.
    getter cookies : Array(CookieInfo) = [] of CookieInfo
    # Whether all crawled pages stayed on the same domain as the start URL.
    getter stayed_on_domain : Bool = false
    # Whether the browser fallback was used for any page in this crawl.
    getter browser_used : Bool = false
  end

  # A URL entry from a sitemap.
  class SitemapUrl
    include JSON::Serializable
    # The URL.
    getter url : String = ""
    # The last modification date, if present.
    getter lastmod : String?
    # The change frequency, if present.
    getter changefreq : String?
    # The priority, if present.
    getter priority : String?
  end

  # The result of a map operation, containing discovered URLs.
  class MapResult
    include JSON::Serializable
    # The list of discovered URLs.
    getter urls : Array(SitemapUrl) = [] of SitemapUrl
  end

  # Rich markdown conversion result from HTML processing.
  class MarkdownResult
    include JSON::Serializable
    # Converted markdown text.
    getter content : String = ""
    # Structured document tree with semantic nodes.
    getter document_structure : JSON::Any?
    # Extracted tables with structured cell data.
    getter tables : Array(JSON::Any) = [] of JSON::Any
    # Non-fatal processing warnings.
    getter warnings : Array(String) = [] of String
    # Whether citation conversion was applied and produced at least one reference.
    #
    # `true` when the markdown contained inline links that were converted to
    # numbered citation references. The converted content (with `[N]` markers)
    # is available in `content`; the full reference list is accessible via
    # `generate_citations` if needed separately.
    getter citations : Bool = false
    # Content-filtered markdown optimized for LLM consumption.
    getter fit_content : String?
  end

  # Information about a link found on a page.
  class LinkInfo
    include JSON::Serializable
    # The resolved URL of the link.
    getter url : String = ""
    # The visible text of the link.
    getter text : String = ""
    # The classification of the link.
    getter link_type : LinkType = LinkType::Internal
    # The `rel` attribute value, if present.
    getter rel : String?
    # Whether the link has `rel="nofollow"`.
    getter nofollow : Bool = false
  end

  # Information about an image found on a page.
  class ImageInfo
    include JSON::Serializable
    # The image URL.
    getter url : String = ""
    # The alt text, if present.
    getter alt : String?
    # The width attribute, if present and parseable.
    getter width : UInt32?
    # The height attribute, if present and parseable.
    getter height : UInt32?
    # The source of the image reference.
    getter source : ImageSource = ImageSource::Img
  end

  # Information about a feed link found on a page.
  class FeedInfo
    include JSON::Serializable
    # The feed URL.
    getter url : String = ""
    # The feed title, if present.
    getter title : String?
    # The type of feed.
    getter feed_type : FeedType = FeedType::Rss
  end

  # A JSON-LD structured data entry found on a page.
  class JsonLdEntry
    include JSON::Serializable
    # The `@type` value from the JSON-LD object.
    getter schema_type : String = ""
    # The `name` value, if present.
    getter name : String?
    # The raw JSON-LD string.
    getter raw : String = ""
  end

  # Information about an HTTP cookie received from a response.
  class CookieInfo
    include JSON::Serializable
    # The cookie name.
    getter name : String = ""
    # The cookie value.
    getter value : String = ""
    # The cookie domain, if specified.
    getter domain : String?
    # The cookie path, if specified.
    getter path : String?
  end

  # A downloaded asset from a page.
  class DownloadedAsset
    include JSON::Serializable
    # The original URL of the asset.
    getter url : String = ""
    # The SHA-256 content hash of the asset.
    getter content_hash : String = ""
    # The MIME type from the Content-Type header.
    getter mime_type : String?
    # The size of the asset in bytes.
    getter size : UInt64 = 0
    # The category of the asset.
    getter asset_category : AssetCategory = AssetCategory::Image
    # The HTML tag that referenced this asset (e.g., "link", "script", "img").
    getter html_tag : String?
  end

  # Article metadata extracted from `article:*` Open Graph tags.
  class ArticleMetadata
    include JSON::Serializable
    # The article publication time.
    getter published_time : String?
    # The article modification time.
    getter modified_time : String?
    # The article author.
    getter author : String?
    # The article section.
    getter section : String?
    # The article tags.
    getter tags : Array(String) = [] of String
  end

  # An hreflang alternate link entry.
  class HreflangEntry
    include JSON::Serializable
    # The language code (e.g., "en", "fr", "x-default").
    getter lang : String = ""
    # The URL for this language variant.
    getter url : String = ""
  end

  # Information about a favicon or icon link.
  class FaviconInfo
    include JSON::Serializable
    # The icon URL.
    getter url : String = ""
    # The `rel` attribute (e.g., "icon", "apple-touch-icon").
    getter rel : String = ""
    # The `sizes` attribute, if present.
    getter sizes : String?
    # The MIME type, if present.
    getter mime_type : String?
  end

  # A heading element extracted from the page.
  class HeadingInfo
    include JSON::Serializable
    # The heading level (1-6).
    getter level : UInt8 = 0
    # The heading text content.
    getter text : String = ""
  end

  # Response metadata extracted from HTTP headers.
  class ResponseMeta
    include JSON::Serializable
    # The ETag header value.
    getter etag : String?
    # The Last-Modified header value.
    getter last_modified : String?
    # The Cache-Control header value.
    getter cache_control : String?
    # The Server header value.
    getter server : String?
    # The X-Powered-By header value.
    getter x_powered_by : String?
    # The Content-Language header value.
    getter content_language : String?
    # The Content-Encoding header value.
    getter content_encoding : String?
  end

  # Metadata extracted from an HTML page's `<meta>` tags and `<title>` element.
  class PageMetadata
    include JSON::Serializable
    # The page title from the `<title>` element.
    getter title : String?
    # The meta description.
    getter description : String?
    # The canonical URL from `<link rel="canonical">`.
    getter canonical_url : String?
    # Keywords from `<meta name="keywords">`.
    getter keywords : String?
    # Author from `<meta name="author">`.
    getter author : String?
    # Viewport content from `<meta name="viewport">`.
    getter viewport : String?
    # Theme color from `<meta name="theme-color">`.
    getter theme_color : String?
    # Generator from `<meta name="generator">`.
    getter generator : String?
    # Robots content from `<meta name="robots">`.
    getter robots : String?
    # The `lang` attribute from the `<html>` element.
    getter html_lang : String?
    # The `dir` attribute from the `<html>` element.
    getter html_dir : String?
    # Open Graph title.
    getter og_title : String?
    # Open Graph type.
    getter og_type : String?
    # Open Graph image URL.
    getter og_image : String?
    # Open Graph description.
    getter og_description : String?
    # Open Graph URL.
    getter og_url : String?
    # Open Graph site name.
    getter og_site_name : String?
    # Open Graph locale.
    getter og_locale : String?
    # Open Graph video URL.
    getter og_video : String?
    # Open Graph audio URL.
    getter og_audio : String?
    # Open Graph locale alternates.
    getter og_locale_alternates : Array(String)?
    # Twitter card type.
    getter twitter_card : String?
    # Twitter title.
    getter twitter_title : String?
    # Twitter description.
    getter twitter_description : String?
    # Twitter image URL.
    getter twitter_image : String?
    # Twitter site handle.
    getter twitter_site : String?
    # Twitter creator handle.
    getter twitter_creator : String?
    # Dublin Core title.
    getter dc_title : String?
    # Dublin Core creator.
    getter dc_creator : String?
    # Dublin Core subject.
    getter dc_subject : String?
    # Dublin Core description.
    getter dc_description : String?
    # Dublin Core publisher.
    getter dc_publisher : String?
    # Dublin Core date.
    getter dc_date : String?
    # Dublin Core type.
    getter dc_type : String?
    # Dublin Core format.
    getter dc_format : String?
    # Dublin Core identifier.
    getter dc_identifier : String?
    # Dublin Core language.
    getter dc_language : String?
    # Dublin Core rights.
    getter dc_rights : String?
    # Article metadata from `article:*` Open Graph tags.
    getter article : ArticleMetadata?
    # Hreflang alternate links.
    getter hreflangs : Array(HreflangEntry)?
    # Favicon and icon links.
    getter favicons : Array(FaviconInfo)?
    # Heading elements (h1-h6).
    getter headings : Array(HeadingInfo)?
    # Computed word count of the page body text.
    getter word_count : UInt64?
  end

  # Request to begin a single-URL streaming crawl.
  #
  # Wraps a single seed URL for delivery through the streaming-adapter binding
  # surface. Required as a struct because alef's streaming adapter requires a
  # named request type — primitives are not supported.
  class CrawlStreamRequest
    include JSON::Serializable
    # The seed URL to crawl.
    getter url : String = ""
  end

  # Request to begin a multi-URL streaming crawl.
  #
  # Wraps a set of seed URLs for delivery through the streaming-adapter binding
  # surface. Required as a struct because alef's streaming adapter requires a
  # named request type — primitives are not supported.
  class BatchCrawlStreamRequest
    include JSON::Serializable
    # The seed URLs to crawl. Each URL is followed independently up to the
    # engine's configured depth.
    getter urls : Array(String) = [] of String
  end

  # Result of citation conversion.
  class CitationResult
    include JSON::Serializable
    # Markdown with links replaced by numbered citations.
    getter content : String = ""
    # Numbered reference list: (index, url, text).
    getter references : Array(CitationReference) = [] of CitationReference
  end

  # A single numbered reference in a citation list — produced by the citation
  # extractor when content uses inline `[N]`-style markers.
  class CitationReference
    include JSON::Serializable
    # 1-based reference number as it appears in the source text.
    getter index : UInt64 = 0
    # Resolved absolute URL for this reference.
    getter url : String = ""
    # Human-readable anchor text or title for the reference.
    getter text : String = ""
  end

  # Opaque handle to a configured crawl engine.
  #
  # Constructed via [`create_engine`] with an optional [`CrawlConfig`].
  # Default implementations for all pluggable components are used internally.
  class CrawlEngineHandle
    # Wraps the owned FFI handle; do not construct directly.
    def initialize(@handle : Void*)
    end
    # Raw handle for passing back across the C ABI.
    def to_unsafe : Void*
      @handle
    end
    def finalize
      LibCberg.crawl_engine_handle_free(@handle) unless @handle.null?
    end

    # Stream of `CrawlEvent` items over a fiber-fed channel.
    def crawl_stream(req : CrawlStreamRequest) : Channel(CrawlEvent)
    __handle_req = LibCberg.crawl_stream_request_from_json(req.to_json)
      __handle = LibCberg.crawl_engine_handle_crawl_stream_start(@handle, __handle_req)
      __ch = Channel(CrawlEvent).new
      raise "LibCberg.crawl_engine_handle_crawl_stream_start returned a null iterator" if __handle.null?
      spawn do
        begin
          loop do
            __chunk = LibCberg.crawl_engine_handle_crawl_stream_next(__handle)
            break if __chunk.null?
            __jp = LibCberg.crawl_event_to_json(__chunk)
            if __jp.null?
              LibCberg.crawl_event_free(__chunk)
              break
            end
            __json = String.new(__jp)
            LibCberg.free_string(__jp)
            LibCberg.crawl_event_free(__chunk)
            __ch.send(CrawlEvent.from_json(__json))
          end
        ensure
          LibCberg.crawl_engine_handle_crawl_stream_free(__handle)
    LibCberg.crawl_stream_request_free(__handle_req)
          __ch.close
        end
      end
      __ch
    end

    # Stream of `CrawlEvent` items over a fiber-fed channel.
    def batch_crawl_stream(req : BatchCrawlStreamRequest) : Channel(CrawlEvent)
    __handle_req = LibCberg.batch_crawl_stream_request_from_json(req.to_json)
      __handle = LibCberg.crawl_engine_handle_batch_crawl_stream_start(@handle, __handle_req)
      __ch = Channel(CrawlEvent).new
      raise "LibCberg.crawl_engine_handle_batch_crawl_stream_start returned a null iterator" if __handle.null?
      spawn do
        begin
          loop do
            __chunk = LibCberg.crawl_engine_handle_batch_crawl_stream_next(__handle)
            break if __chunk.null?
            __jp = LibCberg.crawl_event_to_json(__chunk)
            if __jp.null?
              LibCberg.crawl_event_free(__chunk)
              break
            end
            __json = String.new(__jp)
            LibCberg.free_string(__jp)
            LibCberg.crawl_event_free(__chunk)
            __ch.send(CrawlEvent.from_json(__json))
          end
        ensure
          LibCberg.crawl_engine_handle_batch_crawl_stream_free(__handle)
    LibCberg.batch_crawl_stream_request_free(__handle_req)
          __ch.close
        end
      end
      __ch
    end
  end

  # Result from a single URL in a batch scrape operation.
  class BatchScrapeResult
    include JSON::Serializable
    # The URL that was scraped.
    getter url : String = ""
    # The scrape result, if successful.
    getter result : ScrapeResult?
    # The error message, if the scrape failed.
    getter error : String?
  end

  # Result from a single URL in a batch crawl operation.
  class BatchCrawlResult
    include JSON::Serializable
    # The seed URL that was crawled.
    getter url : String = ""
    # The crawl result, if successful.
    getter result : CrawlResult?
    # The error message, if the crawl failed.
    getter error : String?
  end

  # Aggregate result of a batch scrape, exposing per-URL results plus precomputed counts.
  #
  # The counts are derived once at construction so every binding language can read them
  # as plain integer fields without re-iterating the `results` vector.
  class BatchScrapeResults
    include JSON::Serializable
    # Per-URL scrape results, in the order URLs were submitted.
    getter results : Array(BatchScrapeResult) = [] of BatchScrapeResult
    # Total number of URLs in the batch (equal to `results.len()`).
    getter total_count : UInt64 = 0
    # Number of URLs whose scrape succeeded (`error` is `None`).
    getter completed_count : UInt64 = 0
    # Number of URLs whose scrape failed (`error` is `Some`).
    getter failed_count : UInt64 = 0
  end

  # Aggregate result of a batch crawl, exposing per-URL results plus precomputed counts.
  #
  # The counts are derived once at construction so every binding language can read them
  # as plain integer fields without re-iterating the `results` vector.
  class BatchCrawlResults
    include JSON::Serializable
    # Per-URL crawl results, in the order seed URLs were submitted.
    getter results : Array(BatchCrawlResult) = [] of BatchCrawlResult
    # Total number of seed URLs in the batch (equal to `results.len()`).
    getter total_count : UInt64 = 0
    # Number of seed URLs whose crawl succeeded (`error` is `None`).
    getter completed_count : UInt64 = 0
    # Number of seed URLs whose crawl failed (`error` is `Some`).
    getter failed_count : UInt64 = 0
  end

  # SSRF policy configuration.
  class SsrfPolicy
    include JSON::Serializable
    # If true, reject URLs that resolve to private/metadata IP ranges.
    getter deny_private : Bool = true
    # Maximum number of HTTP redirects to follow during validation.
    getter max_redirects : UInt8 = 5
  end

  # When to use the headless browser fallback.
  enum BrowserMode
    Auto
    Always
    Never
    Stealth
  end

  # Wait strategy for browser page rendering.
  enum BrowserWait
    NetworkIdle
    Selector
    Fixed
  end

  # Browser backend used for JavaScript rendering.
  enum BrowserBackend
    Chromiumoxide
    Native
  end

  # Authentication configuration.
  abstract class AuthConfig
    include JSON::Serializable
    use_json_discriminator "type", {"basic" => AuthConfig::Basic, "bearer" => AuthConfig::Bearer, "header" => AuthConfig::Header}
  end

  class AuthConfig::Basic < AuthConfig
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "basic"
    getter username : String
    getter password : String
  end

  class AuthConfig::Bearer < AuthConfig
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "bearer"
    getter token : String
  end

  class AuthConfig::Header < AuthConfig
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "header"
    getter name : String
    getter value : String
  end

  # The classification of a link.
  enum LinkType
    Internal
    External
    Anchor
    Document
  end

  # The source of an image reference.
  enum ImageSource
    Img
    PictureSource
    OgImage
    TwitterImage
  end

  # The type of a feed (RSS, Atom, or JSON Feed).
  enum FeedType
    Rss
    Atom
    JsonFeed
  end

  # The category of a downloaded asset.
  enum AssetCategory
    Document
    Image
    Audio
    Video
    Font
    Stylesheet
    Script
    Archive
    Data
    Other
  end

  # An event emitted during a streaming crawl operation.
  #
  # Not available on `wasm32` targets — streaming requires native concurrency
  # primitives (tokio channels, `JoinSet`) that are not supported on wasm32.
  #
  # Delivered to bindings through each target's native streaming idiom.
  abstract class CrawlEvent
    include JSON::Serializable
    use_json_discriminator "type", {"page" => CrawlEvent::Page, "error" => CrawlEvent::Error, "complete" => CrawlEvent::Complete}
  end

  class CrawlEvent::Page < CrawlEvent
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "page"
    getter result : CrawlPageResult
  end

  class CrawlEvent::Error < CrawlEvent
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "error"
    getter url : String
    getter error : String
  end

  class CrawlEvent::Complete < CrawlEvent
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "complete"
    getter pages_crawled : UInt64
  end

  # A single page interaction action.
  #
  # Actions are serialized with a `type` tag using camelCase naming,
  # except `ExecuteJs` which is explicitly renamed to `"executeJs"`.
  abstract class PageAction
    include JSON::Serializable
    use_json_discriminator "type", {"click" => PageAction::Click, "type" => PageAction::TypeText, "press" => PageAction::Press, "scroll" => PageAction::Scroll, "wait" => PageAction::Wait, "screenshot" => PageAction::Screenshot, "executeJs" => PageAction::ExecuteJs, "scrape" => PageAction::Scrape}
  end

  class PageAction::Click < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "click"
    getter selector : String
  end

  class PageAction::TypeText < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "type"
    getter selector : String
    getter text : String
  end

  class PageAction::Press < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "press"
    getter key : String
  end

  class PageAction::Scroll < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "scroll"
    getter direction : ScrollDirection
    getter selector : String?
    getter amount : Int64?
  end

  class PageAction::Wait < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "wait"
    getter milliseconds : Int64?
    getter selector : String?
  end

  class PageAction::Screenshot < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "screenshot"
    @[JSON::Field(key: "fullPage")]
    getter full_page : Bool?
  end

  class PageAction::ExecuteJs < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "executeJs"
    getter script : String
  end

  class PageAction::Scrape < PageAction
    include JSON::Serializable
    @[JSON::Field(key: "type")]
    getter type_ : String = "scrape"
  end

  # Direction for a scroll action.
  enum ScrollDirection
    Up
    Down
  end

  # Errors that can occur during crawling, scraping, or mapping operations.
  class CrawlError < Exception
  end

  # SSRF validation error.
  class SsrfError < Exception
  end

  # Convert markdown links to numbered citations.
  def self.generate_citations(markdown : String) : CitationResult
    __ptr = LibCberg.generate_citations(markdown)
    raise "LibCberg.generate_citations returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.citation_result_to_json(__ptr)
    LibCberg.citation_result_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    CitationResult.from_json(__json)
  end

  # Create a new crawl engine with the given configuration.
  def self.create_engine(config : CrawlConfig?) : CrawlEngineHandle
    __handle_config = config.nil? ? Pointer(LibCberg::CrawlConfig).null : LibCberg.crawl_config_from_json(config.not_nil!.to_json)
    __ptr = LibCberg.create_engine(__handle_config)
    raise "LibCberg.create_engine returned a null pointer" if __ptr.null?
    LibCberg.crawl_config_free(__handle_config) unless __handle_config.null?
    CrawlEngineHandle.new(__ptr)
  end

  # Scrape a single URL, returning extracted page data.
  def self.scrape(engine : CrawlEngineHandle, url : String) : ScrapeResult
    __ptr = LibCberg.scrape(engine.to_unsafe, url)
    raise "LibCberg.scrape returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.scrape_result_to_json(__ptr)
    LibCberg.scrape_result_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    ScrapeResult.from_json(__json)
  end

  # Crawl a website starting from `url`, following links up to the configured depth.
  def self.crawl(engine : CrawlEngineHandle, url : String) : CrawlResult
    __ptr = LibCberg.crawl(engine.to_unsafe, url)
    raise "LibCberg.crawl returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.crawl_result_to_json(__ptr)
    LibCberg.crawl_result_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    CrawlResult.from_json(__json)
  end

  # Discover all pages on a website by following links and sitemaps.
  def self.map_urls(engine : CrawlEngineHandle, url : String) : MapResult
    __ptr = LibCberg.map_urls(engine.to_unsafe, url)
    raise "LibCberg.map_urls returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.map_result_to_json(__ptr)
    LibCberg.map_result_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    MapResult.from_json(__json)
  end

  # Execute browser actions on a single page.
  def self.interact(engine : CrawlEngineHandle, url : String, actions : Array(PageAction)) : InteractionResult
    __ptr = LibCberg.interact(engine.to_unsafe, url, actions.to_json)
    raise "LibCberg.interact returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.interaction_result_to_json(__ptr)
    LibCberg.interaction_result_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    InteractionResult.from_json(__json)
  end

  # Scrape multiple URLs concurrently.
  def self.batch_scrape(engine : CrawlEngineHandle, urls : Array(String)) : BatchScrapeResults
    __ptr = LibCberg.batch_scrape(engine.to_unsafe, urls.to_json)
    raise "LibCberg.batch_scrape returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.batch_scrape_results_to_json(__ptr)
    LibCberg.batch_scrape_results_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    BatchScrapeResults.from_json(__json)
  end

  # Crawl multiple seed URLs concurrently, each following links to configured depth.
  def self.batch_crawl(engine : CrawlEngineHandle, urls : Array(String)) : BatchCrawlResults
    __ptr = LibCberg.batch_crawl(engine.to_unsafe, urls.to_json)
    raise "LibCberg.batch_crawl returned a null pointer" if __ptr.null?
    __json_ptr = LibCberg.batch_crawl_results_to_json(__ptr)
    LibCberg.batch_crawl_results_free(__ptr)
    __json = String.new(__json_ptr)
    LibCberg.free_string(__json_ptr)
    BatchCrawlResults.from_json(__json)
  end
end
