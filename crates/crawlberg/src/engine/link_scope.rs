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
    pub(super) stay_on_domain: bool,
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

    let is_document = link.link_type == LinkType::Document;

    if is_document && parent_doc_depth > 0 && !document_link_in_depth_policy(parent_doc_depth, policy) {
        return false;
    }

    host_in_scope(link_url, is_document, policy)
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

/// Whether `link_url`'s host is in scope, given the link's kind.
///
/// ~keep An unparsable `link_url` is admitted here (matching both loops' prior behaviour):
/// the fetch itself will report the failure instead of this check silently swallowing it.
///
/// ~keep `stay_on_domain` governs DOCUMENT links only, and that is not a new invention -- it
/// is the one thing the flag has ever actually done. `classify_link` (html/links.rs) tests the
/// file extension BEFORE it compares hosts, so a cross-host `.pdf`/`.docx`/`.zip` link is
/// `LinkType::Document`, never `External`. Both pre-1.8.0 loops admitted `Document` through
/// their type gate regardless of host, skipped the doc-depth gate entirely at
/// `parent_doc_depth == 0`, and then reached a `if config.stay_on_domain { .. }` block that
/// only rejected the link when the flag was SET. Since it defaults to `false`, a default
/// crawl followed cross-host document links -- the common "PDFs on a CDN or S3" case. An
/// earlier draft of this function applied the host rule to every link type and silently broke
/// that (crawlberg#72); restoring it is what the `is_document` escape below is for.
///
/// ~keep For page links the rule is the crawlberg#60 fix and is deliberately narrow: the seed
/// host, plus its subdomains when `allow_subdomains` is set, and no other host ever. That
/// stays independent of `stay_on_domain`, because making `false` mean "no host restriction"
/// for pages would turn the DEFAULT configuration (`max_depth: None`, `max_pages: None`, both
/// resolving to `usize::MAX`) into an unbounded crawl of the open web.
fn host_in_scope(link_url: &str, is_document: bool, policy: &LinkScopePolicy<'_>) -> bool {
    if is_document && !policy.stay_on_domain {
        return true;
    }
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
        stay_on_domain_policy(allow_subdomains, false, base_host, base_host_suffix)
    }

    fn stay_on_domain_policy<'a>(
        allow_subdomains: bool,
        stay_on_domain: bool,
        base_host: &'a str,
        base_host_suffix: &'a str,
    ) -> LinkScopePolicy<'a> {
        LinkScopePolicy {
            follow_document_urls: false,
            document_url_depth: None,
            allow_subdomains,
            stay_on_domain,
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

    // ~keep Pins the crawlberg#60 contract for PAGE links: the fix must not widen the default
    // crawl beyond the seed host and its subdomains. `stay_on_domain` is deliberately not an
    // input for page links; it governs document links only, per the note on `host_in_scope`.
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

    // ~keep crawlberg#72. `classify_link` matches the extension before it compares hosts, so
    // these are `Document`, not `External`, and pre-1.8.0 followed them by default. A draft of
    // the crawlberg#60 fix applied the page host rule to every link type and silently dropped
    // them; these four pin the restored behaviour in both directions.
    #[test]
    fn should_follow_a_cross_host_document_link_by_default() {
        let policy = policy(false, "example.com", "example.com");
        assert!(
            link_in_scope(
                &link(LinkType::Document),
                "https://cdn.other.test/report.pdf",
                0,
                &policy
            ),
            "a cross-host document link must still be followed by a default crawl, as it was before 1.8.0"
        );
    }

    #[test]
    fn should_reject_a_cross_host_document_link_when_stay_on_domain_is_set() {
        let policy = stay_on_domain_policy(false, true, "example.com", "example.com");
        assert!(
            !link_in_scope(
                &link(LinkType::Document),
                "https://cdn.other.test/report.pdf",
                0,
                &policy
            ),
            "stay_on_domain must confine document links to the seed host"
        );
    }

    #[test]
    fn should_follow_a_same_host_document_link_when_stay_on_domain_is_set() {
        let policy = stay_on_domain_policy(false, true, "example.com", "example.com");
        assert!(
            link_in_scope(&link(LinkType::Document), "https://example.com/report.pdf", 0, &policy),
            "stay_on_domain must not reject a document on the seed host itself"
        );
    }

    #[test]
    fn should_follow_a_subdomain_document_link_when_stay_on_domain_and_subdomains_are_set() {
        let policy = stay_on_domain_policy(true, true, "example.com", "example.com");
        assert!(
            link_in_scope(
                &link(LinkType::Document),
                "https://files.example.com/report.pdf",
                0,
                &policy
            ),
            "allow_subdomains must widen stay_on_domain to subdomain-hosted documents"
        );
    }

    // ~keep stay_on_domain must not become a second, redundant switch for page links: that is
    // what would turn a default crawl (max_depth/max_pages both usize::MAX) into an unbounded
    // crawl of the open web.
    #[test]
    fn should_reject_an_unrelated_host_page_even_when_stay_on_domain_is_false() {
        let policy = stay_on_domain_policy(false, false, "example.com", "example.com");
        assert!(
            !link_in_scope(&link(LinkType::External), "https://anywhere.example/page", 0, &policy),
            "stay_on_domain=false must not admit cross-host PAGE links"
        );
    }
}
