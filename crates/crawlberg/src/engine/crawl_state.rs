//! The in-progress state of one crawl, and the blocking page extraction that feeds it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use url::Url;

use crate::helpers::PathPattern;
use crate::html::{
    HtmlExtraction, PageScan, decode_page, extract_page_data, is_binary_content_type, is_binary_url, is_html_content,
    is_pdf_content, is_pdf_url, mask_raw_text_markup,
};
use crate::tower::BodyText;
use crate::types::*;

use crate::helpers::RobotsOutcome;
use crate::scrape::RobotsDirectives;
use crate::traits::*;

/// Fallback URL used when a fetched URL fails to parse during extraction.
/// This should never happen in practice since the URL was already fetched successfully.
pub(super) static FALLBACK_URL: std::sync::LazyLock<Url> =
    std::sync::LazyLock::new(|| Url::parse("http://invalid").expect("static fallback URL"));

/// The crawl-wide inputs every stage of the loop reads and none of them changes.
///
/// ~keep Bundled rather than threaded as nine separate arguments through
/// `run_crawl_loop` -> `drive_crawl_loop` -> `process_fetch_result` ->
/// `discover_and_enqueue_links`: each hand-off repeated the same list, so adding one
/// input meant editing four signatures and four call sites in lockstep.
pub(super) struct LoopContext<'a> {
    /// ~keep `Arc` rather than a borrowed slice: `fetch_and_extract` is spawned into a
    /// ~keep `JoinSet` and must own a redirect-hop policy of its own (see `FetchResult`'s
    /// ~keep `final_url`), so each spawn needs a cheap, `'static` clone of these lists.
    ///
    /// ~keep `regex::Regex::clone` allocates a fresh, cold cache pool per copy, so holding
    /// these as slices and rebuilding an `Arc` per spawn rebuilt every pattern's cache
    /// once per fetch.
    pub(super) exclude_regexes: Arc<[PathPattern]>,
    pub(super) include_regexes: Arc<[PathPattern]>,
    pub(super) robots: &'a RobotsOutcome,
    pub(super) base_host: &'a str,
    pub(super) base_host_suffix: &'a str,
    pub(super) max_depth: usize,
    pub(super) max_pages: usize,
    pub(super) start_time: Instant,
    pub(super) tx: &'a Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
}

/// Whether this is a streaming crawl whose receiver has been dropped.
pub(super) fn receiver_gone(tx: &Option<tokio::sync::mpsc::Sender<CrawlEvent>>) -> bool {
    tx.as_ref().is_some_and(tokio::sync::mpsc::Sender::is_closed)
}

/// Resolve once a streaming crawl's receiver is dropped; never for a non-streaming crawl.
pub(super) async fn receiver_closed(tx: &Option<tokio::sync::mpsc::Sender<CrawlEvent>>) {
    match tx {
        Some(sender) => sender.closed().await,
        None => std::future::pending().await,
    }
}

/// The page whose links are being discovered, as link discovery sees it.
pub(super) struct ParentPage<'a> {
    pub(super) url: &'a str,
    pub(super) depth: usize,
    /// 0 for a page reached by ordinary HTML navigation, higher for one reached through
    /// consecutive [`LinkType::Document`] hops.
    pub(super) doc_depth: u32,
}

/// Result of a concurrent fetch task, holding everything needed to process a completed fetch.
pub(super) struct FetchResult {
    pub(super) entry: FrontierEntry,
    pub(super) status_code: u16,
    pub(super) content_type: String,
    pub(super) body: String,
    /// Raw response bytes, preserved so non-HTML documents (PDF, …) can be
    /// materialized into a [`DownloadedDocument`](crate::types::DownloadedDocument).
    pub(super) body_bytes: Vec<u8>,
    pub(super) headers: HashMap<String, Vec<String>>,
    pub(super) extraction: HtmlExtraction,
    /// The page's own `noindex` / `nofollow`, from its `X-Robots-Tag` headers and meta tags.
    pub(super) robots: RobotsDirectives,
    pub(super) is_binary: bool,
    pub(super) is_pdf: bool,
    pub(super) detected_charset: Option<String>,
    pub(super) browser_used: bool,
    /// The URL this fetch actually landed on, after following any redirects `entry.url`
    /// pointed at. Equal to `entry.url` when nothing redirected.
    pub(super) final_url: String,
    /// Redirect hops taken to reach `final_url` from `entry.url`.
    pub(super) redirect_count: usize,
    /// The URLs the browser's SSRF check refused for requests the page sent.
    pub(super) ssrf_refused_urls: Vec<String>,
    /// The extraction's read of `body`, taken by the markdown conversion so the page is read once.
    pub(super) page_scan: Option<PageScan>,
}

/// What a spawned frontier fetch produced.
///
/// ~keep A redirect hop refused by policy (robots, `exclude_paths`, or a dedup collision with
/// ~keep a page already claimed elsewhere) is not a fetch failure -- it is exactly the outcome
/// ~keep `should_fetch_url` already reports silently for a frontier entry the policy rejects
/// ~keep before ever spawning it. `Skipped` carries that same silence forward for a rejection
/// ~keep discovered only after the fetch was already running.
pub(super) enum FetchOutcome {
    Fetched(Box<FetchResult>),
    /// A redirect hop was refused by policy (robots, `exclude_paths`, or a dedup collision).
    /// Carries the frontier entry back so the driving loop can retire it from `in_flight`.
    Skipped(FrontierEntry),
}

/// Result of blocking HTML extraction within a fetch task.
pub(super) struct PageExtraction {
    /// The page body, decoded with `detected_charset` when the page is not UTF-8
    /// (see [`blocking_extract_page`]). This is the body extraction,
    /// markdown conversion, and the final `CrawlPageResult::html` must all use —
    /// never the caller's original UTF-8-lossy `body`.
    pub(super) body: String,
    pub(super) body_bytes: Vec<u8>,
    pub(super) extraction: HtmlExtraction,
    pub(super) robots: RobotsDirectives,
    pub(super) is_binary: bool,
    pub(super) is_pdf: bool,
    pub(super) detected_charset: Option<String>,
    /// The read of `body` the extraction used, kept for the markdown.
    pub(super) page_scan: PageScan,
}

/// Mutable state accumulated during a crawl.
pub(super) struct CrawlState {
    pub(super) pages: Vec<CrawlPageResult>,
    pub(super) redirect_count: usize,
    pub(super) error: Option<String>,
    pub(super) was_skipped: bool,
    pub(super) all_cookies: Vec<CookieInfo>,
    pub(super) pages_failed: usize,
    pub(super) urls_discovered: usize,
    pub(super) urls_filtered: usize,
    pub(super) pages_count: usize,
    pub(super) is_streaming: bool,
    /// URLs pushed to the frontier and not yet popped back into the selection window.
    ///
    /// ~keep Tracked locally so the `crawl.frontier_size` span field keeps its published
    /// meaning without calling `Frontier::len()` per dequeue, which would be a round trip
    /// per URL against a remote frontier.
    pub(super) frontier_pending: usize,
}

impl CrawlState {
    pub(super) fn new(capacity: usize, is_streaming: bool) -> Self {
        Self {
            pages: Vec::with_capacity(capacity),
            redirect_count: 0,
            error: None,
            was_skipped: false,
            all_cookies: Vec::new(),
            pages_failed: 0,
            urls_discovered: 0,
            urls_filtered: 0,
            pages_count: 0,
            is_streaming,
            frontier_pending: 0,
        }
    }

    /// Pages completed so far, read from whichever counter this crawl is actually filling.
    ///
    /// ~keep A streaming crawl moves every page into a `CrawlEvent` and never pushes to
    /// `pages`, so `pages.len()` is permanently 0 there. Reading that field directly is what
    /// made the `crawl.pages_completed` span report 0 for every iteration of every streaming
    /// crawl; the three budget/stats callers already branched correctly and the span did not.
    pub(super) fn pages_processed(&self) -> usize {
        if self.is_streaming {
            self.pages_count
        } else {
            self.pages.len()
        }
    }

    pub(super) fn into_result(self, final_url: String) -> CrawlResult {
        let (pages_to_return, stayed_on_domain) = if self.is_streaming {
            (Vec::new(), true)
        } else {
            let stayed = self.pages.iter().all(|p| p.stayed_on_domain);
            (self.pages, stayed)
        };
        CrawlResult::new(CrawlOutcome {
            pages: pages_to_return,
            final_url,
            redirect_count: self.redirect_count,
            was_skipped: self.was_skipped,
            error: self.error,
            cookies: self.all_cookies,
            stayed_on_domain,
        })
    }
}

/// The body of a fetched page, as the fetch returned it.
pub(super) struct FetchedBody {
    pub(super) body: String,
    pub(super) body_bytes: Vec<u8>,
    /// Whether `body` is the text of the page already. See [`BodyText`].
    pub(super) body_text: BodyText,
}

/// Perform HTML extraction in a blocking context.
///
/// The parsed document borrows the input string, so this must run via `spawn_blocking`.
///
/// ~keep Decides the character set of the page from `body_bytes` and decodes them (as
/// `scrape_from_crawl_response` in `scrape.rs` does) *before* parsing, so extraction,
/// markdown conversion, and the `html` field the caller reports downstream all see
/// the text of the page instead of `crawl()`'s original UTF-8-lossy fallback body.
/// `body_text` says whether `body` is that text already, as the body of a browser is.
///
/// `page_scan` is the redirect check's read of `body`, reused unless the decode replaced it.
pub(super) fn blocking_extract_page(
    url: &str,
    content_type: &str,
    header_robots: RobotsDirectives,
    user_agent: &str,
    fetched: FetchedBody,
    page_scan: Option<PageScan>,
) -> PageExtraction {
    let FetchedBody {
        body,
        body_bytes,
        body_text,
    } = fetched;
    let parsed_url = Url::parse(url).unwrap_or_else(|_| FALLBACK_URL.clone());

    let (text, detected_charset) = decode_page(&body_text, content_type, url, &body_bytes);
    let (body, page_scan) = match text {
        Some(text) => (text, None),
        None => (body, page_scan),
    };

    let is_binary = is_binary_content_type(content_type) || is_binary_url(url);
    let is_pdf = is_pdf_content(content_type, &body) || is_pdf_url(url);
    let is_html = is_html_content(content_type, &body);

    // ~keep Parse the masked source, never `body`: `tl` reads the contents of raw-text elements
    // ~keep as markup, which both invents tags and hides real ones.
    let parsed_html = match page_scan {
        Some(page_scan) => page_scan.attach(&body),
        None => mask_raw_text_markup(&body),
    };
    let (extraction, robots) = if let Ok(doc) = crate::html::parse_html(&parsed_html.text) {
        (
            extract_page_data(&doc, &parsed_html, &parsed_url, is_html && !is_binary && !is_pdf, false),
            header_robots.with_meta_tags(&doc, user_agent),
        )
    } else {
        let extraction = HtmlExtraction {
            metadata: PageMetadata::default(),
            links: Vec::new(),
            images: Vec::new(),
            feeds: Vec::new(),
            json_ld: Vec::new(),
        };
        (extraction, header_robots)
    };

    let page_scan = parsed_html.detach();
    PageExtraction {
        body,
        body_bytes,
        extraction,
        robots,
        is_binary,
        is_pdf,
        detected_charset,
        page_scan,
    }
}
