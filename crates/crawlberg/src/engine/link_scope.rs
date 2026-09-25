//! Shared "is this link in scope" decision, used by both the native and wasm crawl loops.
//!
//! ~keep This is deliberately narrow: only the type/doc-depth/host-scope decision is
//! shared. Per-page link caps and frontier dedup+enqueue stay duplicated in each loop
//! (crawl_engineer task for crawlberg#60) because they interleave with loop-specific state
//! (`CrawlState` vs `SequentialState`) that a shared helper would have to abstract over for
//! no real gain.

use crate::types::{LinkInfo, LinkType};

/// Host/domain and document-depth policy applied while deciding whether to follow a link.
///
/// ~keep Bundled into one struct rather than five separate parameters: they always travel
/// together at both call sites and a flat parameter list would exceed the project's
/// max-parameters limit.
pub(super) struct LinkScopePolicy<'a> {
    pub(super) follow_document_urls: bool,
    pub(super) document_url_depth: Option<u32>,
    pub(super) allow_subdomains: bool,
    pub(super) base_host: &'a str,
    pub(super) base_host_suffix: &'a str,
}

/// Whether a discovered link may be followed from a page at `parent_doc_depth`.
///
/// `link_url` must already be fragment-stripped and resolved to an absolute URL — both
/// callers do this before calling in.
///
/// ~keep `LinkType::External` is NOT rejected here based on type alone. `classify_link`
/// (html/links.rs) marks every link whose host differs from the page it was found on as
/// External, independent of `allow_subdomains` — it has no config to consult. Gating on
/// type unconditionally dropped every subdomain link before the host-scope check below
/// ever ran, making `allow_subdomains` inert (crawlberg#60). Only `LinkType::Anchor` (a
/// same-page fragment) is rejected on type.
pub(super) fn link_in_scope(
    link: &LinkInfo,
    link_url: &str,
    parent_doc_depth: u32,
    policy: &LinkScopePolicy<'_>,
) -> bool {
    if link.link_type == LinkType::Anchor {
        return false;
    }

    if link.link_type == LinkType::Document
        && parent_doc_depth > 0
        && !document_link_in_depth_policy(parent_doc_depth, policy)
    {
        return false;
    }

    host_in_scope(link_url, policy)
}

/// Whether a `Document` link discovered from an existing document context (`parent_doc_depth
/// > 0`) may still be followed, per `follow_document_urls`/`document_url_depth`.
fn document_link_in_depth_policy(parent_doc_depth: u32, policy: &LinkScopePolicy<'_>) -> bool {
    if !policy.follow_document_urls {
        return false;
    }
    let child_doc_depth = parent_doc_depth + 1;
    match policy.document_url_depth {
        Some(max_doc_depth) => child_doc_depth <= max_doc_depth,
        None => true,
    }
}

/// Whether `link_url`'s host satisfies `allow_subdomains`.
///
/// ~keep An unparsable `link_url` is admitted here (matching both loops' prior behaviour):
/// the fetch itself will report the failure instead of this check silently swallowing it.
///
/// ~keep This is deliberately ADDITIVE over the behaviour crawlberg has always shipped: the
/// seed host is in scope, subdomains join it when `allow_subdomains` is set (that is
/// crawlberg#60), and no other host is ever admitted. `stay_on_domain` is not consulted,
/// because it has never had an observable effect: the type gate dropped every cross-host link
/// before the old `stay_on_domain` block could run, and for a same-host link that block
/// admitted it either way. Honouring it literally -- `false` meaning "no host restriction" --
/// would turn the DEFAULT configuration (`stay_on_domain: false`, `max_depth: None`,
/// `max_pages: None`, both of which resolve to `usize::MAX` in the crawl loop) into an
/// unbounded crawl of every reachable host, so it needs its own decision and release note
/// rather than arriving inside a subdomain fix. Tracked as crawlberg#72.
fn host_in_scope(link_url: &str, policy: &LinkScopePolicy<'_>) -> bool {
    let Ok(parsed) = url::Url::parse(link_url) else {
        return true;
    };
    let link_host = parsed.host_str().unwrap_or("");
    link_host == policy.base_host || (policy.allow_subdomains && link_host.ends_with(policy.base_host_suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy<'a>(allow_subdomains: bool, base_host: &'a str, base_host_suffix: &'a str) -> LinkScopePolicy<'a> {
        LinkScopePolicy {
            follow_document_urls: false,
            document_url_depth: None,
            allow_subdomains,
            base_host,
            base_host_suffix,
        }
    }

    fn link(link_type: LinkType) -> LinkInfo {
        LinkInfo {
            url: String::new(),
            text: String::new(),
            link_type,
            rel: None,
            nofollow: false,
        }
    }

    #[test]
    fn should_follow_subdomain_link_when_allow_subdomains_is_true() {
        let policy = policy(true, "example.com", "example.com");
        assert!(
            link_in_scope(&link(LinkType::External), "https://docs.example.com/page", 0, &policy),
            "a subdomain External link must be followable when allow_subdomains is true"
        );
    }

    #[test]
    fn should_reject_subdomain_link_when_allow_subdomains_is_false() {
        let policy = policy(false, "example.com", "example.com");
        assert!(
            !link_in_scope(&link(LinkType::External), "https://docs.example.com/page", 0, &policy),
            "a subdomain link must be rejected when allow_subdomains is false"
        );
    }

    #[test]
    fn should_reject_unrelated_host_even_when_allow_subdomains_is_true() {
        let policy = policy(true, "example.com", "example.com");
        assert!(
            !link_in_scope(&link(LinkType::External), "https://other.example/page", 0, &policy),
            "an unrelated host must never pass, regardless of allow_subdomains"
        );
    }

    // ~keep Pins the additive contract: fixing crawlberg#60 must not widen the default crawl
    // beyond the seed host and its subdomains. `stay_on_domain` is deliberately not an input
    // here; see the note on `host_in_scope` and crawlberg#72.
    #[test]
    fn should_reject_an_unrelated_host_when_subdomains_are_not_allowed() {
        let policy = policy(false, "example.com", "example.com");
        assert!(
            !link_in_scope(&link(LinkType::External), "https://anywhere.example/page", 0, &policy),
            "an unrelated host must never be followed by a default-configured crawl"
        );
    }

    #[test]
    fn should_reject_anchor_links_regardless_of_host_scope() {
        let policy = policy(false, "example.com", "example.com");
        assert!(
            !link_in_scope(&link(LinkType::Anchor), "https://example.com/page#section", 0, &policy),
            "a fragment-only anchor is never a page to enqueue"
        );
    }
}
