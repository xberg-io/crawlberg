//! Link discovery: which discovered links survive policy, and in what order they enqueue.

use futures::StreamExt;
use url::Url;

use super::CrawlEngine;
use super::DEFAULT_MAX_LINKS_PER_PAGE;
use super::crawl_state::{CrawlState, LoopContext, ParentPage};
use crate::error::CrawlError;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::normalize::{normalize_url_for_dedup, strip_fragment};
use crate::telemetry::attributes::{CRAWL_DEPTH, CRAWL_LINK_TYPE, CRAWL_PARENT_URL, URL_DOMAIN, URL_FULL};
use crate::traits::*;
use crate::types::*;

/// Outcome of validating one discovered link: `(url, is_document_link, depth)` when it may be
/// enqueued, or `(url, reason)` when it was rejected.
type ValidatedLink = Result<(String, bool, usize), (String, String)>;

impl CrawlEngine {
    /// Discover links from a page and add unseen ones to the working set.
    ///
    /// `parent_doc_depth` is taken from `entry.doc_depth` of the page being processed.
    /// It is 0 for pages reached via ordinary HTML navigation, and > 0 for pages reached
    /// via consecutive `LinkType::Document` hops.
    ///
    /// Called only when the caller has already determined that discovery is appropriate
    /// (i.e. `follow_document_urls` is satisfied for in-document-context pages).
    ///
    /// `LinkType::Internal` links are always enqueued.
    /// `LinkType::Document` links are enqueued when either:
    ///   * parent_doc_depth == 0 (HTML page discovering document URLs — original behaviour),
    ///   * OR `follow_document_urls` is true AND the child doc_depth does not exceed
    ///     `document_url_depth` (if set).
    ///
    /// SSRF validation is applied at enqueue time with bounded concurrency (16 concurrent
    /// DNS lookups). URLs that fail validation are logged as warnings and not enqueued.
    /// Surviving links are pushed onto the frontier in document order.
    pub(super) async fn discover_and_enqueue_links(
        &self,
        links: &[LinkInfo],
        parent: &ParentPage<'_>,
        context: &LoopContext<'_>,
        state: &mut CrawlState,
    ) -> Result<(), CrawlError> {
        let candidates = self.collect_link_candidates(links, parent, context).await?;
        let validated = validate_link_candidates(&self.config.ssrf, candidates).await;
        self.enqueue_validated_links(validated, parent, state).await
    }

    /// Filter `links` down to the ones this crawl may follow, marking each as seen so a
    /// concurrent discovery of the same URL cannot enqueue it twice.
    async fn collect_link_candidates(
        &self,
        links: &[LinkInfo],
        parent: &ParentPage<'_>,
        context: &LoopContext<'_>,
    ) -> Result<Vec<(String, bool, usize)>, CrawlError> {
        let parent_doc_depth = parent.doc_depth;
        let mut candidates = Vec::new();
        let link_cap = self.config.max_links_per_page.unwrap_or(DEFAULT_MAX_LINKS_PER_PAGE);

        for link in links {
            if candidates.len() >= link_cap {
                tracing::warn!(
                    target: "crawlberg.frontier",
                    link_count = links.len(),
                    cap = link_cap,
                    "page link fan-out exceeds cap, truncating discovered links"
                );
                break;
            }

            let is_doc_link = link.link_type == LinkType::Document;

            if link.link_type != LinkType::Internal && !is_doc_link {
                continue;
            }

            // ~keep Document pages can discover more documents only within follow_document_urls/depth policy.
            if is_doc_link && parent_doc_depth > 0 {
                if !self.config.follow_document_urls {
                    continue;
                }
                let child_doc_depth = parent_doc_depth + 1;
                if let Some(max_doc_depth) = self.config.document_url_depth
                    && child_doc_depth > max_doc_depth
                {
                    continue;
                }
            }

            let link_url = strip_fragment(&link.url);

            if self.config.stay_on_domain
                && let Ok(lu) = Url::parse(&link_url)
            {
                let link_host = lu.host_str().unwrap_or("");
                if link_host != context.base_host
                    && (!self.config.allow_subdomains || !link_host.ends_with(context.base_host_suffix))
                {
                    continue;
                }
            }

            let child_depth = parent.depth + 1;
            let dedup_key = normalize_url_for_dedup(&link_url);
            // ~keep Mark seen before SSRF validation so concurrent discovery cannot enqueue dedup-equivalent URLs.
            if !self.frontier.is_seen(&dedup_key).await? {
                self.frontier.mark_seen(&dedup_key).await?;
                candidates.push((link_url, is_doc_link, child_depth));
            }
        }

        Ok(candidates)
    }

    /// Push every link that survived validation onto the frontier, in document order.
    async fn enqueue_validated_links(
        &self,
        validated: Vec<ValidatedLink>,
        parent: &ParentPage<'_>,
        state: &mut CrawlState,
    ) -> Result<(), CrawlError> {
        let parent_doc_depth = parent.doc_depth;

        for result in validated {
            match result {
                Ok((link_url, is_doc_link, child_depth)) => {
                    let child_doc_depth: u32 = if is_doc_link { parent_doc_depth + 1 } else { 0 };
                    let priority = self.strategy.score_url(&link_url, child_depth);

                    {
                        let link_host = Url::parse(&link_url)
                            .ok()
                            .and_then(|u| u.host_str().map(str::to_owned))
                            .unwrap_or_default();
                        // ~keep Both fields are full URLs a crawl may have discovered with
                        // embedded userinfo (http://user:pass@host/); redact before they reach
                        // the span, which is shipped to logs/OTLP by default.
                        let redacted_link_url = crate::net::redact_url_credentials(&link_url);
                        let redacted_parent_url = crate::net::redact_url_credentials(parent.url);
                        let _discover_span = tracing::info_span!(
                            "crawl.page.discover",
                            { URL_FULL } = %redacted_link_url,
                            { URL_DOMAIN } = %link_host,
                            { CRAWL_PARENT_URL } = %redacted_parent_url,
                            { CRAWL_DEPTH } = child_depth as i64,
                            { CRAWL_LINK_TYPE } = if is_doc_link { "document" } else { "internal" },
                        )
                        .entered();
                    }

                    self.push_to_frontier(
                        FrontierEntry {
                            url: link_url.clone(),
                            depth: child_depth,
                            doc_depth: child_doc_depth,
                            priority,
                        },
                        state,
                    )
                    .await?;
                    state.urls_discovered += 1;
                    self.event_emitter.on_discovered(&link_url, child_depth).await;
                }
                Err((link_url, reason)) => {
                    tracing::warn!(
                        url = %link_url,
                        reason = %reason,
                        "link rejected by SSRF policy at enqueue time"
                    );
                }
            }
        }

        Ok(())
    }
}

/// Maximum SSRF validations in flight while checking one page's discovered links.
const SSRF_VALIDATION_CONCURRENCY: usize = 16;

/// Run the SSRF policy over every candidate, preserving document order.
///
/// ~keep `buffered` yields results in *input* order while keeping
/// SSRF_VALIDATION_CONCURRENCY validations in flight. A `JoinSet` drained with
/// `join_next()` yields them in completion order, which made sibling enqueue order
/// nondeterministic and the documented breadth-first traversal unreproducible.
/// Validation is not spawned: `validate_url` awaits `tokio::net::lookup_host`, which
/// offloads the resolver itself, so the loop thread is never blocked.
async fn validate_link_candidates(policy: &SsrfPolicy, candidates: Vec<(String, bool, usize)>) -> Vec<ValidatedLink> {
    futures::stream::iter(candidates.into_iter().map(|(link_url, is_doc_link, child_depth)| {
        let ssrf_policy = policy.clone();
        async move {
            let Ok(url_obj) = url::Url::parse(&link_url) else {
                return Err((link_url, "invalid URL format".to_owned()));
            };

            match validate_url(&url_obj, &ssrf_policy).await {
                Ok(_) => Ok((link_url, is_doc_link, child_depth)),
                Err(e) => Err((link_url, e.to_string())),
            }
        }
    }))
    .buffered(SSRF_VALIDATION_CONCURRENCY)
    .collect()
    .await
}
