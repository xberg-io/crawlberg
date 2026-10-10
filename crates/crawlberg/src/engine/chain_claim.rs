//! The claim one redirect chain makes on the frontier, shared by the native and wasm crawl loops.

use std::collections::HashSet;

use crate::error::CrawlError;
use crate::normalize::normalize_url_for_dedup;
use crate::traits::Frontier;

/// The frontier keys one redirect chain holds: its starting address and every hop it claimed.
///
/// ~keep Deduplicates against the frontier's own seen-set rather than a set local to the
/// ~keep chain: a page reachable both directly (its own frontier entry) and via a redirect
/// ~keep must be reported once, and the frontier is the one place both paths already agree on
/// ~keep what "seen" means.
#[derive(Default)]
pub(super) struct ChainClaim {
    keys: HashSet<String>,
}

impl ChainClaim {
    /// Whether the chain may take `url`. `is_redirect_hop` is `false` for the chain's own
    /// starting address, which the discovery step that enqueued it already claimed.
    ///
    /// ~keep A hop whose key the chain already holds is the chain's own page under another
    /// ~keep address, not a page claimed elsewhere: a server redirects `/docs` to `/docs#top`, or
    /// ~keep to another percent-encoded spelling of `/docs`. The frontier has that key because
    /// ~keep this chain's own entry put it there, so refusing the hop would drop the page the
    /// ~keep chain is fetching, with every page behind it.
    pub(super) async fn admits(
        &mut self,
        frontier: &dyn Frontier,
        include_query: bool,
        url: &str,
        is_redirect_hop: bool,
    ) -> Result<bool, CrawlError> {
        let dedup_key = normalize_url_for_dedup(url, include_query);
        if !is_redirect_hop {
            self.keys.insert(dedup_key);
            return Ok(true);
        }
        if self.keys.contains(&dedup_key) {
            return Ok(true);
        }
        if frontier.is_seen(&dedup_key).await? {
            return Ok(false);
        }
        frontier.mark_seen(&dedup_key).await?;
        self.keys.insert(dedup_key);
        Ok(true)
    }
}
