//! Turning a completed fetch into a [`CrawlPageResult`] and the events that accompany it.

use url::Url;

use super::CrawlEngine;
use super::crawl_state::{CrawlState, FALLBACK_URL, FetchResult, LoopContext, ParentPage};
use super::redirect::url_host;
use crate::error::CrawlError;
use crate::http::extract_cookies_from_hashmap;
use crate::normalize::normalize_url;
use crate::traits::*;
use crate::types::*;

/// Lowest status code treated as a server-side failure rather than a page.
const SERVER_ERROR_STATUS_FLOOR: u16 = 500;

impl CrawlEngine {
    /// Process a completed fetch: extract data, discover links, build page result.
    ///
    /// Returns `true` if the crawl should stop (max_pages reached or receiver dropped).
    pub(super) async fn process_fetch_result(
        &self,
        mut fetch: FetchResult,
        state: &mut CrawlState,
        context: &LoopContext<'_>,
    ) -> Result<bool, CrawlError> {
        let page_url = fetch.entry.url.clone();
        let depth = fetch.entry.depth;

        if fetch.status_code >= SERVER_ERROR_STATUS_FLOOR {
            state.pages_failed += 1;
            self.report_server_error(page_url, fetch.status_code, context).await;
            return Ok(false);
        }

        self.collect_page_cookies(&page_url, &fetch, state);

        // ~keep Taken rather than moved out so `fetch` stays whole: the helpers below borrow it
        // ~keep after this point, and a partial move would make that borrow illegal.
        let mut body = std::mem::take(&mut fetch.body);

        if let Some(max_size) = self.config.max_body_size {
            crate::http::truncate_body_at_char_boundary(&mut body, max_size);
        }
        let body_size = body.len();

        let page_was_skipped = fetch.is_binary || fetch.is_pdf;
        if page_was_skipped {
            state.was_skipped = true;
        }

        let (final_url, page_parsed, norm_url, stayed_on_domain) = resolved_page_location(&fetch, context.base_host);

        self.discover_links_if_allowed(&fetch, &final_url, page_was_skipped, context, state)
            .await?;

        let (downloaded_document, markdown) = self
            .derive_page_content(&final_url, &page_parsed, &fetch, &body, page_was_skipped)
            .await;

        let page = CrawlPageResult {
            url: page_url.clone(),
            normalized_url: norm_url,
            status_code: fetch.status_code,
            content_type: fetch.content_type,
            html: body,
            body_size,
            metadata: fetch.extraction.metadata,
            links: fetch.extraction.links,
            images: fetch.extraction.images,
            feeds: fetch.extraction.feeds,
            json_ld: fetch.extraction.json_ld,
            depth,
            stayed_on_domain,
            was_skipped: page_was_skipped,
            is_pdf: fetch.is_pdf,
            detected_charset: fetch.detected_charset,
            markdown,
            extracted_data: None,
            extraction_meta: None,
            downloaded_document,
            browser_used: fetch.browser_used,
            final_url,
            redirect_count: fetch.redirect_count,
            noindex_detected: fetch.robots.noindex,
            nofollow_detected: fetch.robots.nofollow,
        };

        let page = match self.content_filter.filter(page).await? {
            Some(filtered_page) => filtered_page,
            None => {
                state.urls_filtered += 1;
                return Ok(false);
            }
        };

        Ok(self.deliver_page(page, state, context).await)
    }

    /// Fold this response's `Set-Cookie` headers into the crawl-wide cookie jar.
    fn collect_page_cookies(&self, page_url: &str, fetch: &FetchResult, state: &mut CrawlState) {
        if !self.config.cookies_enabled {
            return;
        }
        let fetch_host = url_host(page_url);
        state
            .all_cookies
            .extend(extract_cookies_from_hashmap(&fetch_host, &fetch.headers));
    }

    /// Enqueue the page's outbound links, unless its depth, its document context or its own
    /// `nofollow` (when the crawl respects robots) says not to.
    async fn discover_links_if_allowed(
        &self,
        fetch: &FetchResult,
        page_url: &str,
        page_was_skipped: bool,
        context: &LoopContext<'_>,
        state: &mut CrawlState,
    ) -> Result<(), CrawlError> {
        let in_document_context = fetch.entry.doc_depth > 0;
        let should_discover = (!page_was_skipped || in_document_context)
            && (self.config.follow_document_urls || !in_document_context)
            && fetch.entry.depth < context.max_depth
            && !(self.config.respect_robots_txt && fetch.robots.nofollow);
        if !should_discover {
            return Ok(());
        }

        let parent = ParentPage {
            url: page_url,
            depth: fetch.entry.depth,
            doc_depth: fetch.entry.doc_depth,
        };
        self.discover_and_enqueue_links(&fetch.extraction.links, &parent, context, state)
            .await
    }

    /// Build the representations derived from a page body: the downloaded-document record,
    /// and the markdown rendering (skipped, like the record's content, for binary/PDF pages).
    async fn derive_page_content(
        &self,
        page_url: &str,
        page_parsed: &Url,
        fetch: &FetchResult,
        body: &str,
        page_was_skipped: bool,
    ) -> (Option<DownloadedDocument>, Option<MarkdownResult>) {
        let downloaded_document = crate::document::build_downloaded_document(
            page_url,
            page_parsed,
            &fetch.content_type,
            &fetch.body_bytes,
            page_was_skipped,
            &self.config,
        )
        .await;

        let markdown = if page_was_skipped {
            None
        } else {
            let content_config = crate::scrape::merged_content_config(&self.config);
            crate::markdown::convert_to_markdown(body, &content_config).await
        };

        (downloaded_document, markdown)
    }

    /// Emit the error events for a page whose fetch came back 5xx.
    async fn report_server_error(&self, page_url: String, status_code: u16, context: &LoopContext<'_>) {
        let error_msg = format!("server_error: HTTP {status_code}");
        self.event_emitter
            .on_error(&ErrorEvent {
                url: page_url.clone(),
                error: error_msg.clone(),
            })
            .await;
        let _ = self
            .store
            .store_error(&page_url, &CrawlError::server_error(error_msg.clone()))
            .await;
        let error_event = CrawlEvent::Error {
            url: page_url,
            error: error_msg,
        };
        if let Some(sender) = context.tx {
            let _ = sender.send(error_event.clone()).await;
        }
        if let Some(ref sink) = self.event_sink {
            sink.emit(error_event).await;
        }
    }

    /// Record a finished page everywhere it is owed, and report whether that page was the
    /// one that ends the crawl -- `max_pages` reached, or a streaming receiver gone away.
    async fn deliver_page(&self, page: CrawlPageResult, state: &mut CrawlState, context: &LoopContext<'_>) -> bool {
        self.strategy.on_page_processed(&page);
        let _ = self.store.store_crawl_page(&page.url, &page).await;

        self.event_emitter
            .on_page(&PageEvent {
                url: page.url.clone(),
                status_code: page.status_code,
                depth: page.depth,
            })
            .await;

        if let Some(sender) = context.tx {
            let page_event = CrawlEvent::Page { result: Box::new(page) };
            if sender.send(page_event.clone()).await.is_err() {
                return true;
            }
            if let Some(ref sink) = self.event_sink {
                sink.emit(page_event).await;
            }
            state.pages_count += 1;
            if state.pages_count >= context.max_pages {
                return true;
            }
        } else {
            // ~keep Only clone the page when a sink will actually consume it; a plain
            // crawl() has no sink and would otherwise deep-copy every page and drop it.
            if let Some(ref sink) = self.event_sink {
                sink.emit(CrawlEvent::Page {
                    result: Box::new(page.clone()),
                })
                .await;
            }
            state.pages.push(page);
            if state.pages.len() >= context.max_pages {
                return true;
            }
        }

        false
    }
}

/// Where this fetch's content actually came from, and what it implies for the page.
///
/// ~keep `final_url` is where the content actually came from -- equal to `fetch.entry.url`
/// ~keep unless the fetch redirected. Domain checks, link discovery's base URL, and the
/// ~keep downloaded-document record all use it, so a redirected page's relative links
/// ~keep resolve against the origin that served them rather than the URL originally
/// ~keep requested.
fn resolved_page_location(fetch: &FetchResult, base_host: &str) -> (String, Url, String, bool) {
    let final_url = fetch.final_url.clone();
    let page_parsed = Url::parse(&final_url).unwrap_or_else(|_| FALLBACK_URL.clone());
    let domain = page_parsed.host_str().unwrap_or("");
    let norm_url = normalize_url(&final_url);
    let stayed_on_domain = domain == base_host;
    (final_url, page_parsed, norm_url, stayed_on_domain)
}
