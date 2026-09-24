//! The in-progress state of one crawl, and the blocking page extraction that feeds it.

use std::collections::HashMap;
use std::time::Instant;

use tl::ParserOptions;
use url::Url;

use crate::html::{
    HtmlExtraction, detect_charset, extract_page_data, is_binary_content_type, is_binary_url, is_html_content,
    is_pdf_content, is_pdf_url,
};
use crate::types::*;
use regex::Regex;

use crate::helpers::RobotsOutcome;
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
    pub(super) exclude_regexes: &'a [Regex],
    pub(super) include_regexes: &'a [Regex],
    pub(super) robots: &'a RobotsOutcome,
    pub(super) base_host: &'a str,
    pub(super) base_host_suffix: &'a str,
    pub(super) max_depth: usize,
    pub(super) max_pages: usize,
    pub(super) start_time: Instant,
    pub(super) tx: &'a Option<tokio::sync::mpsc::Sender<CrawlEvent>>,
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
    pub(super) is_binary: bool,
    pub(super) is_pdf: bool,
    pub(super) detected_charset: Option<String>,
    pub(super) browser_used: bool,
}

/// Result of blocking HTML extraction within a fetch task.
pub(super) struct PageExtraction {
    /// The page body, re-decoded with `detected_charset` when a non-UTF-8 encoding
    /// was detected (see [`blocking_extract_page`]). This is the body extraction,
    /// markdown conversion, and the final `CrawlPageResult::html` must all use —
    /// never the caller's original UTF-8-lossy `body`.
    pub(super) body: String,
    pub(super) body_bytes: Vec<u8>,
    pub(super) extraction: HtmlExtraction,
    pub(super) is_binary: bool,
    pub(super) is_pdf: bool,
    pub(super) detected_charset: Option<String>,
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

/// Perform HTML extraction in a blocking context.
///
/// `tl::parse` borrows the input string, so this must run via `spawn_blocking`.
///
/// ~keep Re-decodes `body` from `body_bytes` using the detected charset (mirrors
/// `scrape_from_crawl_response` in `scrape.rs`) *before* parsing, so extraction,
/// markdown conversion, and the `html` field the caller reports downstream all see
/// correctly decoded text instead of `crawl()`'s original UTF-8-lossy fallback body.
/// `detect_charset` runs on `body_bytes` (not `body`) so a byte-order mark or non-ASCII
/// meta tag survives even when `body` is already lossy-mangled.
pub(super) fn blocking_extract_page(
    url: &str,
    content_type: &str,
    body: String,
    body_bytes: Vec<u8>,
) -> PageExtraction {
    let parsed_url = Url::parse(url).unwrap_or_else(|_| FALLBACK_URL.clone());

    let detected_charset = detect_charset(content_type, &body_bytes);
    let body = match detected_charset.as_deref() {
        Some(charset) => crate::http::redecode_with_charset(charset, &body_bytes).unwrap_or(body),
        None => body,
    };

    let is_binary = is_binary_content_type(content_type) || is_binary_url(url);
    let is_pdf = is_pdf_content(content_type, &body) || is_pdf_url(url);
    let is_html = is_html_content(content_type, &body);

    let extraction = if let Ok(doc) = tl::parse(&body, ParserOptions::default()) {
        extract_page_data(&doc, &body, &parsed_url, is_html && !is_binary && !is_pdf, false)
    } else {
        HtmlExtraction {
            metadata: PageMetadata::default(),
            links: Vec::new(),
            images: Vec::new(),
            feeds: Vec::new(),
            json_ld: Vec::new(),
        }
    };

    PageExtraction {
        body,
        body_bytes,
        extraction,
        is_binary,
        is_pdf,
        detected_charset,
    }
}
