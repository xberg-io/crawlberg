//! Sitemap XML parsing and recursive fetching.
//!
//! Substrate-level surface for sitemap.xml — usable without the full crawl
//! engine. The synchronous parsers ([`parse_sitemap_xml`],
//! [`parse_sitemap_index`], [`is_sitemap_index`]) are public so OSS users
//! can build their own fetcher on top. The async recursive fetch helpers
//! (`fetch_sitemap_tree`, `process_sitemap_response`) remain engine-internal
//! because they depend on the engine's HTTP layer and config; substrate users
//! supply their own HTTP and call the parsers directly.
//!
//! ```
//! use crawlberg::sitemap::{parse_sitemap_xml, is_sitemap_index};
//!
//! let body = r#"<?xml version="1.0"?>
//! <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
//!   <url><loc>https://example.com/a</loc></url>
//!   <url><loc>https://example.com/b</loc></url>
//! </urlset>"#;
//! assert!(!is_sitemap_index(body));
//! let urls = parse_sitemap_xml(body);
//! assert_eq!(urls.len(), 2);
//! ```

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, Event};
use url::Url;

use crate::html::is_fetchable_scheme;
use crate::http::http_fetch_sitemap;
use crate::map::MapFilter;
use crate::normalize::resolve_redirect;
use crate::types::{CrawlConfig, SitemapUrl};

/// Which text-bearing child of a `<url>` entry the reader is currently inside.
#[derive(Clone, Copy)]
enum UrlField {
    Loc,
    LastMod,
    ChangeFreq,
    Priority,
}

/// Append the text an entity reference stands for to `buffer`.
///
/// ~keep quick-xml reports entity references as standalone `Event::GeneralRef` events
/// rather than folding them into the surrounding `Event::Text`, so a reference both
/// splits an element's character data and carries a character of its own. Dropping
/// these events loses that character: `?a=1&amp;b=2` would rejoin as `?a=1b=2`.
/// An entity this parser cannot resolve — a document may declare its own in a DTD —
/// contributes nothing, matching how the surrounding parser skips what it cannot read.
fn push_entity_ref(buffer: &mut String, entity: &BytesRef<'_>) {
    match entity.resolve_char_ref() {
        Ok(Some(character)) => buffer.push(character),
        Ok(None) => {
            if let Some(text) = resolve_predefined_entity(entity.as_ref()) {
                buffer.push_str(text);
            }
        }
        // ~keep A character reference quick-xml cannot decode contributes nothing, as
        // ~keep documented above; there is no partial character to salvage.
        Err(_) => {}
    }
}

/// The `<url>` entry currently being read, committed to the result on `</url>`.
#[derive(Default)]
struct UrlEntry {
    loc: String,
    lastmod: Option<String>,
    changefreq: Option<String>,
    priority: Option<String>,
}

impl UrlEntry {
    /// Discard any partially-read entry so a new `<url>` starts clean.
    fn reset(&mut self) {
        self.loc.clear();
        self.lastmod = None;
        self.changefreq = None;
        self.priority = None;
    }

    fn set(&mut self, field: UrlField, value: &str) {
        match field {
            UrlField::Loc => self.loc = value.to_owned(),
            UrlField::LastMod => self.lastmod = Some(value.to_owned()),
            UrlField::ChangeFreq => self.changefreq = Some(value.to_owned()),
            UrlField::Priority => self.priority = Some(value.to_owned()),
        }
    }

    /// The finished entry, or `None` when the entry carried no `<loc>`.
    fn build(&self) -> Option<SitemapUrl> {
        if self.loc.is_empty() {
            return None;
        }
        Some(SitemapUrl {
            url: self.loc.clone(),
            lastmod: self.lastmod.clone(),
            changefreq: self.changefreq.clone(),
            priority: self.priority.clone(),
        })
    }
}

/// Parse a sitemap XML document and extract URL entries.
pub fn parse_sitemap_xml(body: &str) -> Vec<SitemapUrl> {
    let mut urls = Vec::new();

    let mut reader = Reader::from_str(body);
    let mut buf = Vec::new();
    let mut in_url = false;
    let mut current_field: Option<UrlField> = None;
    // ~keep Character data arrives in several events whenever entity references appear
    // ~keep inside an element, so text is accumulated here and only committed to a field
    // ~keep on the closing tag.
    let mut text = String::new();
    let mut entry = UrlEntry::default();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let field = match e.name().as_ref() {
                    "url" => {
                        in_url = true;
                        entry.reset();
                        None
                    }
                    "loc" if in_url => Some(UrlField::Loc),
                    "lastmod" if in_url => Some(UrlField::LastMod),
                    "changefreq" if in_url => Some(UrlField::ChangeFreq),
                    "priority" if in_url => Some(UrlField::Priority),
                    _ => None,
                };
                if field.is_some() {
                    text.clear();
                    current_field = field;
                }
            }
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "url" => {
                    if in_url && let Some(finished) = entry.build() {
                        urls.push(finished);
                    }
                    in_url = false;
                    current_field = None;
                }
                "loc" | "lastmod" | "changefreq" | "priority" => {
                    let value = text.trim();
                    if !value.is_empty()
                        && let Some(field) = current_field
                    {
                        entry.set(field, value);
                    }
                    current_field = None;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref e)) if current_field.is_some() => {
                text.push_str(&e.xml_content(XmlVersion::default()));
            }
            Ok(Event::GeneralRef(ref e)) if current_field.is_some() => push_entity_ref(&mut text, e),
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    urls
}

/// Parse a sitemap index XML document and return the child sitemap URLs.
pub fn parse_sitemap_index(body: &str) -> Vec<String> {
    let mut child_urls = Vec::new();
    let mut reader = Reader::from_str(body);
    let mut buf = Vec::new();
    let mut in_sitemap = false;
    let mut in_loc = false;
    let mut text = String::new();
    let mut current_loc = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                "sitemap" => {
                    in_sitemap = true;
                    current_loc.clear();
                }
                "loc" if in_sitemap => {
                    in_loc = true;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "sitemap" => {
                    if in_sitemap && !current_loc.is_empty() {
                        child_urls.push(current_loc.clone());
                    }
                    in_sitemap = false;
                }
                "loc" => {
                    if in_loc {
                        current_loc = text.trim().to_owned();
                    }
                    in_loc = false;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref e)) if in_loc => {
                text.push_str(&e.xml_content(XmlVersion::default()));
            }
            Ok(Event::GeneralRef(ref e)) if in_loc => push_entity_ref(&mut text, e),
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    child_urls
}

/// Check whether the body looks like a sitemap index (contains `<sitemapindex`).
pub fn is_sitemap_index(body: &str) -> bool {
    body.contains("<sitemapindex") || body.contains("<sitemapindex>")
}

/// Recursively fetch a sitemap tree, following sitemap index references.
///
/// If the URL points to a sitemap index, fetches each child sitemap and
/// collects all URL entries. Handles gzip-compressed sitemaps.
///
/// `filter` and `limit` are applied incrementally as entries are parsed: only
/// matching URLs are retained, and fetching stops once `limit` matching URLs
/// have been collected. This bounds both peak memory and network work on large
/// sitemap-index trees.
pub(crate) async fn fetch_sitemap_tree(
    sitemap_url: &str,
    context: &SitemapWalkContext<'_>,
    limit: Option<usize>,
) -> Vec<SitemapUrl> {
    let resp = match http_fetch_sitemap(sitemap_url, context.config, context.client).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    process_sitemap_response(
        &SitemapDocument {
            url: sitemap_url,
            final_url: &resp.final_url,
            body: &resp.body,
            body_bytes: &resp.body_bytes,
        },
        context,
        limit,
    )
    .await
}

/// Everything a sitemap tree walk needs besides the document itself: the crawl
/// config, the HTTP client used for child fetches, and the compiled URL filter.
pub(crate) struct SitemapWalkContext<'a> {
    pub(crate) config: &'a CrawlConfig,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) filter: &'a MapFilter,
    /// The parsed address of every urlset entry the walk has returned so far.
    ///
    /// ~keep One context serves one `map()` call, so this deduplicates across every document
    /// ~keep that call reads. It sits behind a `Mutex` because the context is shared by
    /// ~keep reference across awaits; the lock is only taken inside synchronous code.
    seen_entries: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl<'a> SitemapWalkContext<'a> {
    pub(crate) fn new(config: &'a CrawlConfig, client: &'a reqwest::Client, filter: &'a MapFilter) -> Self {
        Self {
            config,
            client,
            filter,
            seen_entries: std::sync::Mutex::default(),
        }
    }
}

/// An already-fetched sitemap document: where it came from and what came back.
pub(crate) struct SitemapDocument<'a> {
    /// The URL the walk requested, and the key its visited set records.
    pub(crate) url: &'a str,
    /// The URL that served the document after any redirects. Every `<loc>` resolves against it.
    pub(crate) final_url: &'a str,
    pub(crate) body: &'a str,
    pub(crate) body_bytes: &'a [u8],
}

/// Maximum sitemap-index nesting depth followed before giving up on a branch.
///
/// Bounds worst-case fetch work independently of the visited-set cycle guard,
/// since a long acyclic chain of indexes would otherwise pass the cycle check
/// while still growing unbounded.
const MAX_SITEMAP_INDEX_DEPTH: u32 = 10;

/// Maximum number of child sitemaps followed from a single index document.
const MAX_SITEMAP_INDEX_CHILDREN: usize = 100;

/// Maximum number of distinct sitemap documents fetched across a whole tree walk.
///
/// ~keep Depth and per-tier breadth bound the tree's *shape*, not its size: 100 distinct
/// children at each of 10 tiers is 100^10 fetches, and the visited set stops only repeats,
/// never distinct URLs. `map_limit` does not help — it bounds the URLs *returned*, so an
/// index tree whose leaves are all empty or all filtered out never reaches it and keeps
/// fetching. This is the only bound on total fetch work, so it counts documents the walk
/// commits to fetching rather than the ones it successfully parses.
const MAX_SITEMAP_DOCUMENTS: usize = 1_000;

/// Process an already-fetched sitemap response body, following sitemap index
/// references if needed. Avoids re-fetching a URL that was already retrieved.
///
/// `filter` and `limit` bound the result incrementally: entries that do not
/// match `filter` are discarded as they are parsed, and both child-sitemap
/// fetching and per-child parsing stop once `limit` matching URLs are collected.
///
/// Nested sitemap indexes (an index that itself points at another index) are
/// followed recursively, bounded by [`MAX_SITEMAP_INDEX_DEPTH`] and a visited
/// set of already-fetched URLs so a self-referential sitemap cannot loop
/// forever.
pub(crate) async fn process_sitemap_response(
    document: &SitemapDocument<'_>,
    context: &SitemapWalkContext<'_>,
    limit: Option<usize>,
) -> Vec<SitemapUrl> {
    let mut visited = std::collections::HashSet::new();
    visited.insert(document.url.to_owned());
    process_sitemap_response_inner(document, context, limit, 0, &mut visited).await
}

/// The XML to parse for a fetched document: a body that starts with the gzip magic is
/// inflated, as [`is_gzip`] decides for `map()`'s direct fetch and for [`reads_as_sitemap`];
/// anything else is the body as received. A gzip payload that fails to inflate falls back
/// to the raw body rather than aborting the walk.
fn sitemap_xml_body<'a>(document: &SitemapDocument<'a>) -> std::borrow::Cow<'a, str> {
    inflated(is_gzip(document.body_bytes), document.body_bytes, document.body)
}

/// The inflated payload when `gzip` holds and the payload inflates, else `body` as received.
fn inflated<'a>(gzip: bool, body_bytes: &[u8], body: &'a str) -> std::borrow::Cow<'a, str> {
    if gzip {
        match decompress_gzip(body_bytes) {
            Ok(decompressed) => std::borrow::Cow::Owned(decompressed),
            Err(_) => std::borrow::Cow::Borrowed(body),
        }
    } else {
        std::borrow::Cow::Borrowed(body)
    }
}

/// Gzip member header magic (RFC 1952 §2.3.1).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Whether `map` inflates a fetched body before it reads it: the body starts with the gzip magic.
///
/// ~keep A gzip content type or a `.gz` URL adds nothing: a body without the magic does not
/// ~keep inflate, and one with it inflates whatever its content type and URL say.
pub(crate) fn is_gzip(body_bytes: &[u8]) -> bool {
    body_bytes.starts_with(&GZIP_MAGIC)
}

/// Whether a fetched body is a sitemap document: the XML `map` parses, inflated when [`is_gzip`]
/// says so, reads to the end as one `urlset` or `sitemapindex` root with nothing outside it,
/// holds text only inside an entry's fields, and yields at least one entry to the parsers.
///
/// ~keep This is what the fetch asks before a WAF fingerprint may refuse a sitemap: a `<loc>` can
/// say anything, such as "/blog/why-we-blocked-the-old-api" (crawlberg#515). A block page fails
/// here on its root element (HTML, a CDN's XML error, JSON or text), on text outside an entry's
/// fields, on markup the XML reader cannot close, or on carrying no `<loc>` entry.
pub(crate) fn reads_as_sitemap(body_bytes: &[u8], body: &str) -> bool {
    let xml = inflated(is_gzip(body_bytes), body_bytes, body);
    has_sitemap_shape(&xml) && (!parse_sitemap_xml(&xml).is_empty() || !parse_sitemap_index(&xml).is_empty())
}

/// Whether `xml` has no top-level element but one `urlset` or `sitemapindex`, no text outside the
/// fields of its entries, and reads to the end without an XML error. A body with no root at all
/// passes here and fails the entry check in [`reads_as_sitemap`].
fn has_sitemap_shape(xml: &str) -> bool {
    /// The depth of an entry's fields: root, then `url` or `sitemap`, then `loc` and its siblings.
    const FIELD_DEPTH: usize = 3;
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth: usize = 0;
    let mut read_root = false;
    // Whether the element open at depth 2 is a `url` or `sitemap` entry.
    let mut in_entry = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if depth == 0 => {
                if read_root || !matches!(e.name().as_ref(), "urlset" | "sitemapindex") {
                    return false;
                }
                read_root = true;
                depth = 1;
            }
            Ok(Event::Start(ref e)) if depth == 1 => {
                in_entry = matches!(e.name().as_ref(), "url" | "sitemap");
                depth = 2;
            }
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Empty(_)) if depth == 0 => return false,
            Ok(Event::GeneralRef(_) | Event::CData(_)) if depth < FIELD_DEPTH || !in_entry => return false,
            Ok(Event::Text(ref e))
                if (depth < FIELD_DEPTH || !in_entry) && !e.xml_content(XmlVersion::default()).trim().is_empty() =>
            {
                return false;
            }
            Err(_) => return false,
            Ok(Event::Eof) => return depth == 0,
            _ => {}
        }
        buf.clear();
    }
}

/// Parse a urlset document served from `document_url`, the URL after any redirects, keeping
/// only entries the walk's filter accepts and stopping once `limit` of them have been collected.
///
/// ~keep Each `<loc>` resolves the same way as a sitemap-index child, but keeps its fragment:
/// ~keep an entry on another host is returned on that host. The entry
/// ~keep is returned in the parser's normalized form, so a relative `<loc>` becomes absolute and
/// ~keep two spellings of one address become one entry. An address the walk already returned,
/// ~keep from this document or an earlier one, is skipped before it counts toward `limit`. A
/// ~keep `<loc>` that does not parse is dropped, and so is one that names the sitemap itself or
/// ~keep one whose scheme is not `http` or `https`: map reports only addresses a crawl can fetch.
pub(crate) fn collect_urlset_entries(
    document_url: &str,
    xml_body: &str,
    context: &SitemapWalkContext<'_>,
    limit: Option<usize>,
) -> Vec<SitemapUrl> {
    let document = Url::parse(document_url).ok();
    let mut seen = context
        .seen_entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut urls = Vec::new();
    for mut entry in parse_sitemap_xml(xml_body) {
        let Some(resolved) = resolve_redirect(document_url, &entry.url) else {
            log_unparseable_loc(document_url, document.is_some(), entry.url.len(), "urlset entry");
            continue;
        };
        if !is_fetchable_scheme(&resolved) || names_the_sitemap_itself(&entry.url, &resolved, document.as_ref()) {
            continue;
        }
        entry.url = resolved.into();
        if !context.filter.matches(&entry.url) || !seen.insert(entry.url.clone()) {
            continue;
        }
        urls.push(entry);
        if limit.is_some_and(|limit| urls.len() >= limit) {
            break;
        }
    }
    urls
}

/// Whether a urlset `<loc>` is dropped as not a page. A `<loc>` that is only a query is
/// dropped because it is query-only, not because it names the sitemap document. A `<loc>`
/// that resolves to `document`'s own address once fragments are ignored is dropped because
/// it does name the sitemap document.
fn names_the_sitemap_itself(loc: &str, resolved: &Url, document: Option<&Url>) -> bool {
    if loc.trim_start().starts_with('?') {
        return true;
    }
    let Some(document) = document else {
        return false;
    };
    let mut resolved = resolved.clone();
    resolved.set_fragment(None);
    let mut document = document.clone();
    document.set_fragment(None);
    resolved == document
}

/// Log a sitemap `<loc>` that failed to parse and is skipped. The `<loc>` is logged by
/// length only. `source_url` is the document the `<loc>` came from.
///
/// ~keep `redact_url_credentials` returns an address that does not parse unchanged, so a
/// ~keep `source_url` that does not parse is logged by length only too.
fn log_unparseable_loc(source_url: &str, source_url_parses: bool, loc_len: usize, loc_kind: &'static str) {
    if source_url_parses {
        tracing::debug!(
            sitemap_url = %crate::net::redact_url_credentials(source_url),
            target_len = loc_len,
            loc_kind,
            "sitemap <loc> failed to parse; skipping it"
        );
    } else {
        tracing::debug!(
            sitemap_url_len = source_url.len(),
            target_len = loc_len,
            loc_kind,
            "sitemap <loc> failed to parse; skipping it"
        );
    }
}

/// Whether the walk has already committed to fetching [`MAX_SITEMAP_DOCUMENTS`] documents.
fn document_budget_exhausted(sitemap_url: &str, visited: &std::collections::HashSet<String>) -> bool {
    // ~keep `visited` holds every document the walk has committed to fetching, root
    // included, and is shared across the whole recursion — so its length is the
    // running total this cap is expressed in. An index that answered from another address
    // after a redirect counts twice, once for each address.
    if visited.len() < MAX_SITEMAP_DOCUMENTS {
        return false;
    }
    tracing::warn!(
        sitemap_url = %crate::net::redact_url_credentials(sitemap_url),
        fetched = visited.len(),
        max_documents = MAX_SITEMAP_DOCUMENTS,
        "stopping sitemap walk: fetched the maximum number of sitemap documents"
    );
    true
}

/// Resolve one child `<loc>` of a sitemap index against the index's own URL, without its
/// fragment. `None` when `child_url` cannot be resolved against `sitemap_url` at all, or resolves
/// to a scheme other than `http` or `https`, which the caller treats the same as a child it could
/// not fetch. `sitemap_url_parses` says whether
/// `sitemap_url` parsed, which decides how the refusal is logged.
///
/// ~keep `sitemap_url` is the URL that served the index after redirects, so a relative child
/// ~keep resolves against the host that served the index. An absolute child keeps its own host,
/// ~keep as the sitemaps.org protocol allows; the SSRF policy gates each child fetch, and seed
/// ~keep credentials go only to the seed host.
///
/// ~keep Every path parses `child_url` before it is fetched or used as a dedup key. When
/// ~keep `sitemap_url` itself failed to parse, `resolve_redirect` still parses `child_url` on
/// ~keep its own and refuses it if that fails too, instead of handing back unparsed text. The
/// ~keep fragment never reaches the server, so two children differing only by fragment are one
/// ~keep fetch target and one dedup key.
fn resolve_child_sitemap_url(sitemap_url_parses: bool, sitemap_url: &str, child_url: &str) -> Option<String> {
    let Some(mut resolved) = resolve_redirect(sitemap_url, child_url) else {
        log_unparseable_loc(sitemap_url, sitemap_url_parses, child_url.len(), "sitemap-index child");
        return None;
    };
    if !is_fetchable_scheme(&resolved) {
        return None;
    }
    resolved.set_fragment(None);
    Some(resolved.into())
}

/// Fetch one child sitemap named by an index and walk whatever it turns out to be.
///
/// ~keep A child that cannot be fetched contributes nothing rather than aborting the
/// walk: one unreachable child must not lose the URLs its siblings carry. `depth` is
/// the *parent's* depth; the child is walked one tier deeper.
async fn fetch_child_sitemap(
    child_url: &str,
    context: &SitemapWalkContext<'_>,
    limit: Option<usize>,
    depth: u32,
    visited: &mut std::collections::HashSet<String>,
) -> Vec<SitemapUrl> {
    let Ok(child_resp) = http_fetch_sitemap(child_url, context.config, context.client).await else {
        return Vec::new();
    };

    Box::pin(process_sitemap_response_inner(
        &SitemapDocument {
            url: child_url,
            final_url: &child_resp.final_url,
            body: &child_resp.body,
            body_bytes: &child_resp.body_bytes,
        },
        context,
        limit,
        depth + 1,
        visited,
    ))
    .await
}

/// Recursive worker behind [`process_sitemap_response`]. See that function's
/// docs for the general contract; `depth` and `visited` are the recursion
/// guards threaded through nested sitemap-index fetches.
async fn process_sitemap_response_inner(
    document: &SitemapDocument<'_>,
    context: &SitemapWalkContext<'_>,
    limit: Option<usize>,
    depth: u32,
    visited: &mut std::collections::HashSet<String>,
) -> Vec<SitemapUrl> {
    let xml_source = sitemap_xml_body(document);
    let xml_body = xml_source.as_ref();

    let reached_limit = |len: usize| limit.is_some_and(|limit| len >= limit);

    if !is_sitemap_index(xml_body) {
        return collect_urlset_entries(document.final_url, xml_body, context, limit);
    }

    // ~keep An index reached through a redirect is also recorded under the address that served
    // ~keep it, so a child naming that address is a cycle rather than a second fetch.
    visited.insert(document.final_url.to_owned());

    if depth >= MAX_SITEMAP_INDEX_DEPTH {
        tracing::warn!(
            sitemap_url = %crate::net::redact_url_credentials(document.url),
            depth,
            max_depth = MAX_SITEMAP_INDEX_DEPTH,
            "skipping sitemap index tier: max nesting depth exceeded"
        );
        return Vec::new();
    }

    let child_urls = parse_sitemap_index(xml_body);
    let final_url_parses = Url::parse(document.final_url).is_ok();
    let mut all_urls = Vec::new();
    for child_url in child_urls.iter().take(MAX_SITEMAP_INDEX_CHILDREN) {
        if reached_limit(all_urls.len()) {
            break;
        }
        if document_budget_exhausted(document.url, visited) {
            break;
        }
        let Some(resolved) = resolve_child_sitemap_url(final_url_parses, document.final_url, child_url) else {
            continue;
        };

        if !visited.insert(resolved.clone()) {
            tracing::warn!(
                sitemap_url = %crate::net::redact_url_credentials(&resolved),
                "skipping sitemap index tier: cycle detected (already visited)"
            );
            continue;
        }

        let remaining = limit.map(|limit| limit.saturating_sub(all_urls.len()));
        let child_entries = fetch_child_sitemap(&resolved, context, remaining, depth, visited).await;

        for entry in child_entries {
            all_urls.push(entry);
            if reached_limit(all_urls.len()) {
                break;
            }
        }
    }
    all_urls
}

/// Decompress gzip-encoded data into a UTF-8 string.
///
/// Limits decompressed output to 50 MB to prevent gzip bomb attacks.
pub(crate) fn decompress_gzip(data: &[u8]) -> Result<String, std::io::Error> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    const MAX_DECOMPRESSED_SIZE: u64 = 50 * 1024 * 1024;

    let decoder = GzDecoder::new(data);
    let mut limited = decoder.take(MAX_DECOMPRESSED_SIZE);
    let mut result = String::new();
    limited.read_to_string(&mut result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::MapFilter;
    use crate::tracing_capture::{assert_logged_without_secret, capture_events};
    use crate::types::CrawlConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A `CrawlConfig` that allows fetching the wiremock server on `127.0.0.1`
    /// without tripping SSRF private-network protections.
    fn local_test_config() -> CrawlConfig {
        CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }
    }

    fn walk_context<'a>(
        config: &'a CrawlConfig,
        client: &'a reqwest::Client,
        filter: &'a MapFilter,
    ) -> SitemapWalkContext<'a> {
        SitemapWalkContext::new(config, client, filter)
    }

    fn xml_document<'a>(url: &'a str, body: &'a str) -> SitemapDocument<'a> {
        SitemapDocument {
            url,
            final_url: url,
            body,
            body_bytes: body.as_bytes(),
        }
    }

    fn sitemap_index_xml(child_locs: &[&str]) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for loc in child_locs {
            body.push_str(&format!("<sitemap><loc>{loc}</loc></sitemap>"));
        }
        body.push_str("</sitemapindex>");
        body
    }

    /// A urlset or sitemapindex document with an entry reads as a sitemap whatever its URLs say;
    /// HTML, a CDN's XML error, JSON, text, an empty sitemap and text outside an entry's fields do
    /// not.
    #[test]
    fn reads_as_sitemap_takes_a_sitemap_document_and_nothing_else() {
        let urlset = "<?xml version=\"1.0\"?>\n<!-- generated -->\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"><url><loc>https://example.com/why-we-blocked-it</loc></url></urlset>\n";
        let index = "<sitemapindex><sitemap><loc>/blocked.xml</loc></sitemap></sitemapindex>";
        for (label, body, expected) in [
            ("a urlset with a declaration and a comment", urlset.to_owned(), true),
            ("a sitemap index", index.to_owned(), true),
            ("an empty urlset", "<urlset/>".to_owned(), false),
            ("a urlset with no entry", "<urlset></urlset>".to_owned(), false),
            (
                "a sitemap index with no child",
                "<sitemapindex><sitemap></sitemap></sitemapindex>".to_owned(),
                false,
            ),
            (
                "a urlset that holds block text",
                "<urlset><url><loc>/a</loc></url>Access blocked</urlset>".to_owned(),
                false,
            ),
            (
                "an entry that holds block text",
                "<urlset><url>Access blocked<loc>/a</loc></url></urlset>".to_owned(),
                false,
            ),
            (
                "a CDN's XML error",
                "<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>Request blocked</Message></Error>"
                    .to_owned(),
                false,
            ),
            (
                "an HTML page with a loc element",
                "<html><body><loc>https://example.com/</loc><h1>Access blocked</h1></body></html>".to_owned(),
                false,
            ),
            ("a JSON body", "{\"error\":\"blocked\",\"urlset\":[]}".to_owned(), false),
            (
                "a text block page that names urlset",
                "Access blocked: <urlset><url><loc>/a</loc></url></urlset>".to_owned(),
                false,
            ),
            (
                "a CDATA section of block text before the root",
                "<![CDATA[Access blocked]]><urlset><url><loc>/a</loc></url></urlset>".to_owned(),
                false,
            ),
            (
                "an entry that holds a CDATA section of block text",
                "<urlset><url><![CDATA[Access blocked]]><loc>/a</loc></url></urlset>".to_owned(),
                false,
            ),
            (
                "a urlset with a div of block text after its entry",
                "<urlset><url><loc>/a</loc></url><div><h1>Access blocked</h1></div></urlset>".to_owned(),
                false,
            ),
            (
                "a urlset with a CDATA section of block text in a div's paragraph",
                "<urlset><url><loc>/a</loc></url><div><p><![CDATA[Access blocked]]></p></div></urlset>".to_owned(),
                false,
            ),
            (
                "a urlset with an entity in a div's paragraph",
                "<urlset><url><loc>/a</loc></url><div><p>&lt;</p></div></urlset>".to_owned(),
                false,
            ),
            (
                "an entry field that holds an entity and a CDATA section",
                "<urlset><url><loc>/a?b=1&amp;c=2</loc><news><![CDATA[Title]]></news></url></urlset>".to_owned(),
                true,
            ),
            (
                "an empty element before the root",
                format!("<br/>{}", "<urlset><url><loc>/a</loc></url></urlset>"),
                false,
            ),
            (
                "an entity before the root",
                "&lt;<urlset><url><loc>/a</loc></url></urlset>".to_owned(),
                false,
            ),
            (
                "an HTML page",
                "<html><body><h1>Access blocked</h1></body></html>".to_owned(),
                false,
            ),
            (
                "an XHTML page with a doctype",
                "<!DOCTYPE html><html><body>blocked</body></html>".to_owned(),
                false,
            ),
            (
                "a urlset with markup after it",
                format!("{urlset}<h1>blocked</h1>"),
                false,
            ),
            ("a urlset with text after it", format!("{urlset}Access blocked"), false),
            ("two urlset roots", format!("{urlset}{urlset}"), false),
            (
                "a urlset that does not close",
                "<urlset><url><loc>/blocked</loc></url>".to_owned(),
                false,
            ),
            (
                "a urlset followed by a comment that does not close",
                "<urlset><url><loc>/a</loc></url></urlset><!-- blocked".to_owned(),
                false,
            ),
            ("text", "Status: blocked".to_owned(), false),
            ("an empty body", String::new(), false),
        ] {
            assert_eq!(
                reads_as_sitemap(body.as_bytes(), &body),
                expected,
                "{label}: reads as a sitemap must be {expected}"
            );
        }
    }

    /// A body that starts with the gzip magic is inflated before it is read, as `map` inflates it,
    /// whatever its content type.
    #[test]
    fn reads_as_sitemap_inflates_a_gzip_body_as_map_does() {
        use std::io::Write;
        let gzip = |text: &str| {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(text.as_bytes()).expect("gzip must encode");
            encoder.finish().expect("gzip must finish")
        };
        let urlset = gzip("<urlset><url><loc>https://example.com/why-we-blocked-it</loc></url></urlset>");
        let block_page = gzip("<html><body><h1>Sorry, you have been blocked</h1></body></html>");
        for (label, bytes, expected) in [
            ("a gzip urlset", &urlset, true),
            ("a gzip block page", &block_page, false),
        ] {
            let lossy = String::from_utf8_lossy(bytes);
            assert_eq!(
                reads_as_sitemap(bytes, &lossy),
                expected,
                "{label}: reads as a sitemap must be {expected}"
            );
        }
        assert!(
            decompress_gzip(b"<urlset><url><loc>/a</loc></url></urlset>").is_err(),
            "a body without the gzip magic must not inflate, whatever its content type or URL"
        );
    }

    #[test]
    fn a_sitemap_index_child_the_crawler_cannot_fetch_is_skipped() {
        let sitemap_url = "https://example.com/sitemap-index.xml";
        for child in [
            "file:///etc/sitemap.xml",
            "ftp://example.com/sitemap.xml",
            "blob:https://example.com/x",
        ] {
            let resolved = resolve_child_sitemap_url(true, sitemap_url, child);
            assert!(resolved.is_none(), "for {child}, got {resolved:?}");
        }
        assert_eq!(
            resolve_child_sitemap_url(true, sitemap_url, "child.xml").as_deref(),
            Some("https://example.com/child.xml")
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn an_unparseable_sitemap_index_child_loc_is_refused_not_followed_raw() {
        let sitemap_url = "https://example.com/sitemap-index.xml";
        let resolved = resolve_child_sitemap_url(true, sitemap_url, "https://ex ample.com/bad.xml");

        assert!(
            resolved.is_none(),
            "a sitemap-index child <loc> that fails to parse must not be followed as raw text, \
             got {resolved:?}"
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn an_unparseable_sitemap_index_child_loc_with_credentials_is_never_logged() {
        let sitemap_url = "https://example.com/sitemap-index.xml";
        let (resolved, fields) = capture_events(|| {
            resolve_child_sitemap_url(true, sitemap_url, "https://user:hunter2@ex ample.com/bad.xml")
        });

        assert!(
            resolved.is_none(),
            "an unparseable sitemap-index child <loc> must not be followed"
        );
        assert_logged_without_secret(&fields, "hunter2", sitemap_url);
    }

    #[test]
    fn same_host_child_loc_with_stray_whitespace_normalizes_instead_of_round_tripping_raw() {
        let sitemap_url = "https://example.com/sitemap-index.xml";
        let resolved = resolve_child_sitemap_url(true, sitemap_url, "HTTPS://example.com:443/a\tb.xml");

        assert_eq!(
            resolved,
            Some("https://example.com/ab.xml".to_owned()),
            "a same-host child <loc> must be fetched and deduped on its normalized form, not \
             the raw text, got {resolved:?}"
        );
    }

    #[test]
    fn no_base_child_loc_with_stray_whitespace_normalizes_instead_of_round_tripping_raw() {
        // ~keep `sitemap_url` fails to parse, matching how the caller derives `false`
        // ~keep from `Url::parse(document.final_url).is_ok()`.
        let resolved = resolve_child_sitemap_url(false, "not a url", "HTTPS://example.com:443/a\tb.xml");

        assert_eq!(
            resolved,
            Some("https://example.com/ab.xml".to_owned()),
            "a child <loc> must be parsed and normalized even when the index URL itself has \
             no usable base, got {resolved:?}"
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn no_base_unparseable_child_loc_is_refused_not_returned_raw() {
        let resolved = resolve_child_sitemap_url(false, "not a url", "https://ex ample.com/bad.xml");

        assert!(
            resolved.is_none(),
            "a child <loc> that fails to parse must be refused even when the index URL has no \
             usable base, not returned as raw text, got {resolved:?}"
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn no_base_unparseable_child_loc_with_credentials_is_never_logged() {
        let (resolved, fields) = capture_events(|| {
            resolve_child_sitemap_url(false, "not a url", "https://user:hunter2@ex ample.com/bad.xml")
        });

        assert!(
            resolved.is_none(),
            "an unparseable child <loc> must not be followed even with no usable base"
        );
        assert_logged_without_secret(&fields, "hunter2", &"not a url".len().to_string());
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn no_base_index_url_with_credentials_is_never_logged() {
        // ~keep The index URL itself fails to parse, so the credential redactor cannot
        // ~keep hide its userinfo; the reviewer's own example.
        let sitemap_url = "https://user:hunter2@ba d.com/index.xml";

        let (resolved, fields) =
            capture_events(|| resolve_child_sitemap_url(false, sitemap_url, "https://ex ample.com/bad.xml"));

        assert!(
            resolved.is_none(),
            "an unparseable child <loc> must not be followed even with no usable base"
        );
        assert_logged_without_secret(&fields, "hunter2", &sitemap_url.len().to_string());
        assert!(
            fields
                .iter()
                .any(|(name, value)| name == "sitemap_url_len" && *value == sitemap_url.len().to_string()),
            "the index URL must be logged by length, got {fields:?}"
        );
    }

    async fn mount_xml(mock: &MockServer, route: &str, body: String) {
        let response = ResponseTemplate::new(200)
            .set_body_string(body)
            .append_header("content-type", "application/xml");
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(mock)
            .await;
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_follows_nested_sitemap_index() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep root index -> child index -> grandchild urlset (two tiers of nesting).
        mount_xml(&mock, "/root.xml", sitemap_index_xml(&[&format!("{base}/child.xml")])).await;
        mount_xml(
            &mock,
            "/child.xml",
            sitemap_index_xml(&[&format!("{base}/grandchild.xml")]),
        )
        .await;
        mount_xml(&mock, "/grandchild.xml", urlset(2)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/root.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(
            urls.len(),
            2,
            "URLs from a doubly-nested sitemap index (index -> index -> urlset) must not be dropped, got {urls:?}"
        );
        assert!(
            urls.iter().all(|u| u.url.contains("example.com/page-")),
            "expected grandchild urlset entries, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_dedupes_children_differing_only_by_fragment() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep Same absolute address, same host as the index, differing only by fragment:
        // ~keep the fragment never reaches the server, so this is one document, not two.
        mount_xml(
            &mock,
            "/root.xml",
            sitemap_index_xml(&[&format!("{base}/a.xml"), &format!("{base}/a.xml#x")]),
        )
        .await;
        mount_xml(&mock, "/a.xml", urlset(1)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/root.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(
            urls.len(),
            1,
            "two sitemap-index children differing only by a fragment must dedupe to one \
             fetch, got {urls:?}"
        );

        // ~keep The length check passes on #332's entry dedup alone; the GET count is what proves one fetch.
        let requests = mock.received_requests().await.expect("wiremock records requests");
        let a_xml_hits = requests.iter().filter(|req| req.url.path() == "/a.xml").count();
        assert_eq!(
            a_xml_hits, 1,
            "expected exactly one GET /a.xml, got {a_xml_hits} across {requests:?}"
        );
    }

    /// GETs of `/a.xml` when the index at `/root.xml` lists `child_locs`.
    async fn a_xml_gets_for_index_children(child_locs: impl Fn(&str) -> Vec<String>) -> usize {
        let mock = MockServer::start().await;
        let base = mock.uri();
        let locs = child_locs(&base);
        let loc_refs: Vec<&str> = locs.iter().map(String::as_str).collect();
        mount_xml(&mock, "/root.xml", sitemap_index_xml(&loc_refs)).await;
        mount_xml(&mock, "/a.xml", urlset(1)).await;
        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        fetch_sitemap_tree(
            &format!("{base}/root.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        let requests = mock.received_requests().await.expect("wiremock records requests");
        requests.iter().filter(|req| req.url.path() == "/a.xml").count()
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_fetches_relative_children_differing_only_by_fragment_once() {
        let hits = a_xml_gets_for_index_children(|_| vec!["/a.xml".to_owned(), "/a.xml#x".to_owned()]).await;

        assert_eq!(hits, 1, "expected exactly one GET /a.xml, got {hits}");
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_fetches_a_relative_and_an_absolute_child_differing_only_by_fragment_once() {
        let hits = a_xml_gets_for_index_children(|base| vec!["/a.xml#x".to_owned(), format!("{base}/a.xml")]).await;

        assert_eq!(hits, 1, "expected exactly one GET /a.xml, got {hits}");
    }

    #[tokio::test]
    // ~keep Serial with the redaction capture tests: tracing caches callsite interest per warning.
    #[serial_test::serial(sitemap_redaction_log)]
    async fn fetch_sitemap_tree_terminates_on_self_referential_cycle() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_xml(&mock, "/cycle.xml", sitemap_index_xml(&[&format!("{base}/cycle.xml")])).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/cycle.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert!(
            urls.is_empty(),
            "a self-referential sitemap index must terminate with no URLs, not loop forever, got {urls:?}"
        );
    }

    #[tokio::test]
    // ~keep Serial with the redaction capture tests: tracing caches callsite interest per warning.
    #[serial_test::serial(sitemap_redaction_log)]
    async fn fetch_sitemap_tree_terminates_on_mutual_cycle() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_xml(&mock, "/a.xml", sitemap_index_xml(&[&format!("{base}/b.xml")])).await;
        mount_xml(&mock, "/b.xml", sitemap_index_xml(&[&format!("{base}/a.xml")])).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/a.xml"), &walk_context(&config, &client, &filter), None).await;

        assert!(
            urls.is_empty(),
            "a mutual two-hop sitemap index cycle must terminate with no URLs, not loop forever, got {urls:?}"
        );
    }

    /// The sitemap fetch reads the sitemap it asked for and does not follow a `Refresh` header on
    /// it, as the robots.txt fetch does not.
    #[tokio::test]
    async fn fetch_sitemap_tree_does_not_follow_a_refresh() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        Mock::given(method("GET"))
            .and(path("/sitemap.xml"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "application/xml")
                    .append_header("refresh", "0; url=/elsewhere.xml")
                    .set_body_string(urlset(1)),
            )
            .mount(&mock)
            .await;
        mount_xml(&mock, "/elsewhere.xml", urlset(2)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/sitemap.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        let urls: Vec<String> = urls.into_iter().map(|entry| entry.url).collect();
        assert_eq!(
            urls,
            vec!["https://example.com/page-0".to_owned()],
            "the sitemap fetch must read the sitemap it asked for, not follow its refresh"
        );
    }

    #[tokio::test]
    // ~keep Serial with the redaction capture tests: tracing caches callsite interest per warning.
    #[serial_test::serial(sitemap_redaction_log)]
    async fn fetch_sitemap_tree_stops_at_max_index_depth() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep Chain of MAX_SITEMAP_INDEX_DEPTH + 2 distinct index tiers ending in a
        // ~keep real urlset leaf, so the leaf is only reachable by exceeding the cap.
        let chain_len = MAX_SITEMAP_INDEX_DEPTH as usize + 2;
        for level in 0..chain_len {
            let next = if level + 1 < chain_len {
                format!("{base}/level{}.xml", level + 1)
            } else {
                format!("{base}/leaf.xml")
            };
            mount_xml(&mock, &format!("/level{level}.xml"), sitemap_index_xml(&[&next])).await;
        }
        mount_xml(&mock, "/leaf.xml", urlset(1)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/level0.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert!(
            urls.is_empty(),
            "a sitemap-index chain deeper than MAX_SITEMAP_INDEX_DEPTH must be cut off \
             before reaching the leaf, got {urls:?}"
        );
    }

    #[tokio::test]
    // ~keep Serial with the redaction capture tests: tracing caches callsite interest per warning.
    #[serial_test::serial(sitemap_redaction_log)]
    async fn fetch_sitemap_tree_stops_at_max_sitemap_documents() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep A tree that is shallow (depth 2, well under MAX_SITEMAP_INDEX_DEPTH) and
        // ~keep narrow per tier (100 children, exactly the per-tier cap), so neither
        // ~keep existing bound fires. Only the aggregate document cap can stop it.
        let tier_one: Vec<String> = (0..MAX_SITEMAP_INDEX_CHILDREN)
            .map(|i| format!("{base}/tier1-{i}.xml"))
            .collect();
        mount_xml(
            &mock,
            "/root.xml",
            sitemap_index_xml(&tier_one.iter().map(String::as_str).collect::<Vec<_>>()),
        )
        .await;

        // ~keep Every tier-1 index but the last points at 100 children that are never
        // ~keep mounted: they resolve, get counted, fail to fetch, and burn budget. The
        // ~keep single real leaf hangs off the LAST tier-1 index, so it is reachable only
        // ~keep if the walk is still fetching after ~9,900 dead children.
        for (index, _) in tier_one.iter().enumerate() {
            let is_last = index + 1 == MAX_SITEMAP_INDEX_CHILDREN;
            let children: Vec<String> = if is_last {
                vec![format!("{base}/leaf.xml")]
            } else {
                (0..MAX_SITEMAP_INDEX_CHILDREN)
                    .map(|child| format!("{base}/dead-{index}-{child}.xml"))
                    .collect()
            };
            mount_xml(
                &mock,
                &format!("/tier1-{index}.xml"),
                sitemap_index_xml(&children.iter().map(String::as_str).collect::<Vec<_>>()),
            )
            .await;
        }
        mount_xml(&mock, "/leaf.xml", urlset(1)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(
            &format!("{base}/root.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert!(
            urls.is_empty(),
            "the walk must stop after MAX_SITEMAP_DOCUMENTS fetches, long before reaching \
             the leaf behind ~9,900 dead children, got {urls:?}"
        );
        assert!(
            mock.received_requests()
                .await
                .is_some_and(|requests| requests.len() <= MAX_SITEMAP_DOCUMENTS),
            "the walk must issue at most MAX_SITEMAP_DOCUMENTS ({MAX_SITEMAP_DOCUMENTS}) fetches"
        );
    }

    fn urlset(count: usize) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for i in 0..count {
            body.push_str(&format!("<url><loc>https://example.com/page-{i}</loc></url>"));
        }
        body.push_str("</urlset>");
        body
    }

    #[tokio::test]
    async fn process_sitemap_response_stops_at_limit_on_single_sitemap() {
        let config = CrawlConfig::default();
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = urlset(1000);

        let urls = process_sitemap_response(
            &xml_document("https://example.com/sitemap.xml", &body),
            &walk_context(&config, &client, &filter),
            Some(10),
        )
        .await;

        assert_eq!(
            urls.len(),
            10,
            "map_limit must cap the parsed URLs, not just the returned slice"
        );
    }

    #[tokio::test]
    async fn process_sitemap_response_returns_all_when_unlimited() {
        let config = CrawlConfig::default();
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = urlset(25);

        let urls = process_sitemap_response(
            &xml_document("https://example.com/sitemap.xml", &body),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(urls.len(), 25);
    }

    /// The walk inflates a document that starts with the gzip magic before it parses it.
    #[tokio::test]
    async fn process_sitemap_response_inflates_a_gzip_document() {
        use std::io::Write;
        let config = CrawlConfig::default();
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = urlset(3);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body.as_bytes()).expect("gzip must encode");
        let gzip = encoder.finish().expect("gzip must finish");
        let lossy = String::from_utf8_lossy(&gzip);

        let urls = process_sitemap_response(
            &SitemapDocument {
                url: "https://example.com/sitemap.xml.gz",
                final_url: "https://example.com/sitemap.xml.gz",
                body: &lossy,
                body_bytes: &gzip,
            },
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(urls.len(), 3, "a gzip urlset of three entries must yield three entries");
    }

    /// crawlberg#534: the walk must inflate a gzip body on its magic bytes, as `map()`'s direct
    /// fetch and `reads_as_sitemap` already do (#520), whatever the content type says.
    #[tokio::test]
    async fn fetch_sitemap_tree_inflates_a_gzip_walk_target_served_with_the_wrong_content_type() {
        use std::io::Write;
        let mock = MockServer::start().await;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(urlset(1).as_bytes()).expect("gzip must encode");
        let gzip = encoder.finish().expect("gzip must finish");
        Mock::given(method("GET"))
            .and(path("/feeds/w.xml.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "application/octet-stream")
                    .set_body_bytes(gzip),
            )
            .mount(&mock)
            .await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();
        let urls = fetch_sitemap_tree(
            &format!("{}/feeds/w.xml.gz", mock.uri()),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(
            urls.len(),
            1,
            "a gzip body at a .gz address with content-type application/octet-stream must still \
             be inflated and read, got {urls:?}"
        );
    }

    /// crawlberg#534: a sitemap-index child inflates on its magic bytes too, with no `.gz`
    /// suffix and a content type that does not say gzip.
    #[tokio::test]
    async fn fetch_sitemap_tree_inflates_a_gzip_index_child_served_with_the_wrong_content_type() {
        use std::io::Write;
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_xml(
            &mock,
            "/root.xml",
            sitemap_index_xml(&[&format!("{base}/kids/child.xml")]),
        )
        .await;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(urlset(1).as_bytes()).expect("gzip must encode");
        let gzip = encoder.finish().expect("gzip must finish");
        Mock::given(method("GET"))
            .and(path("/kids/child.xml"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "application/xml")
                    .set_body_bytes(gzip),
            )
            .mount(&mock)
            .await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();
        let urls = fetch_sitemap_tree(
            &format!("{base}/root.xml"),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(
            urls.len(),
            1,
            "a gzip index child with no .gz suffix and a content type that does not say gzip \
             must still be inflated and read, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn process_sitemap_response_applies_filter_before_limit() {
        let config = CrawlConfig {
            map_search: Some("keep".to_string()),
            ..CrawlConfig::default()
        };
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = concat!(
            r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#,
            "<url><loc>https://example.com/keep-1</loc></url>",
            "<url><loc>https://example.com/drop-1</loc></url>",
            "<url><loc>https://example.com/keep-2</loc></url>",
            "<url><loc>https://example.com/drop-2</loc></url>",
            "</urlset>",
        );

        let urls = process_sitemap_response(
            &xml_document("https://example.com/sitemap.xml", body),
            &walk_context(&config, &client, &filter),
            None,
        )
        .await;

        assert_eq!(urls.len(), 2);
        assert!(urls.iter().all(|entry| entry.url.contains("keep")));
    }

    /// The walk applies a host-anchored exclude pattern against the full URL before `limit`
    /// truncates it, so a limited map is not filled with entries the setting was meant to drop.
    #[tokio::test]
    async fn process_sitemap_response_applies_full_url_exclude_filter_before_limit() {
        let config = CrawlConfig {
            exclude_paths: vec![r"^https://example\.com/private/".to_owned()],
            path_patterns_match_url: true,
            ..CrawlConfig::default()
        };
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = concat!(
            r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#,
            "<url><loc>https://example.com/private/one</loc></url>",
            "<url><loc>https://example.org/private/two</loc></url>",
            "</urlset>",
        );

        let urls = process_sitemap_response(
            &xml_document("https://example.com/sitemap.xml", body),
            &walk_context(&config, &client, &filter),
            Some(1),
        )
        .await;

        assert_eq!(
            urls.iter().map(|entry| entry.url.clone()).collect::<Vec<_>>(),
            vec!["https://example.org/private/two".to_owned()],
            "the excluded example.com entry must not fill the one slot `limit` allows"
        );
    }

    /// A sitemap address whose password must never reach a log field.
    const CREDENTIALED_SITEMAP_URL: &str = "https://user:hunter2@example.com/sitemap.xml";

    /// `Visit` that keeps the value of every `sitemap_url` field.
    struct SitemapUrlVisitor<'a>(&'a mut Vec<String>);

    impl tracing::field::Visit for SitemapUrlVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "sitemap_url" {
                self.0.push(format!("{value:?}"));
            }
        }
    }

    /// Minimal `tracing::Subscriber` that records the `sitemap_url` field of every event.
    struct SitemapUrlCapture(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl tracing::Subscriber for SitemapUrlCapture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut values = self.0.lock().expect("sink mutex must not be poisoned");
            event.record(&mut SitemapUrlVisitor(&mut values));
        }

        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn capture_sitemap_urls() -> (
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        tracing::subscriber::DefaultGuard,
    ) {
        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let guard = tracing::subscriber::set_default(SitemapUrlCapture(sink.clone()));
        (sink, guard)
    }

    fn assert_sitemap_url_redacted(sink: &std::sync::Mutex<Vec<String>>) {
        let values = sink.lock().expect("sink mutex must not be poisoned");
        assert!(!values.is_empty(), "expected a warning with a 'sitemap_url' field");
        for value in values.iter() {
            assert!(!value.contains("hunter2"), "the password reached the log: '{value}'");
            assert!(
                value.contains("***:***@example.com/"),
                "expected the redacted address, got '{value}'"
            );
        }
    }

    #[test]
    #[serial_test::serial(sitemap_redaction_log)]
    fn document_budget_warning_redacts_the_sitemap_url() {
        let (sink, _guard) = capture_sitemap_urls();
        let visited: std::collections::HashSet<String> = (0..MAX_SITEMAP_DOCUMENTS)
            .map(|i| format!("https://example.com/{i}.xml"))
            .collect();

        assert!(document_budget_exhausted(CREDENTIALED_SITEMAP_URL, &visited));
        assert_sitemap_url_redacted(&sink);
    }

    #[tokio::test]
    #[serial_test::serial(sitemap_redaction_log)]
    async fn index_depth_warning_redacts_the_sitemap_url() {
        // ~keep #[tokio::test] defaults to a current-thread runtime, so the walk stays on
        // the thread the subscriber guard was set on.
        let (sink, _guard) = capture_sitemap_urls();
        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();
        let body = sitemap_index_xml(&["https://example.com/child.xml"]);

        let urls = process_sitemap_response_inner(
            &xml_document(CREDENTIALED_SITEMAP_URL, &body),
            &walk_context(&config, &client, &filter),
            None,
            MAX_SITEMAP_INDEX_DEPTH,
            &mut std::collections::HashSet::new(),
        )
        .await;

        assert!(
            urls.is_empty(),
            "an index past the depth cap must not be walked, got {urls:?}"
        );
        assert_sitemap_url_redacted(&sink);
    }

    #[tokio::test]
    #[serial_test::serial(sitemap_redaction_log)]
    async fn cycle_warning_never_logs_the_userinfo_of_the_index_or_the_child() {
        let (sink, _guard) = capture_sitemap_urls();
        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();
        // ~keep Both the index and the child carry a password; the child keeps its own host
        // ~keep and loses its userinfo before the cycle check.
        let child = "https://user:hunter2@other.example/child.xml";
        let body = sitemap_index_xml(&[child]);
        // ~keep The child is already visited, so the walk logs the cycle and never fetches it.
        let mut visited =
            std::collections::HashSet::from([
                resolve_child_sitemap_url(true, CREDENTIALED_SITEMAP_URL, child).expect("the child resolves")
            ]);

        let urls = process_sitemap_response_inner(
            &xml_document(CREDENTIALED_SITEMAP_URL, &body),
            &walk_context(&config, &client, &filter),
            None,
            0,
            &mut visited,
        )
        .await;

        assert!(
            urls.is_empty(),
            "an already visited child must not be walked, got {urls:?}"
        );
        let values = sink.lock().expect("sink mutex must not be poisoned");
        assert_eq!(
            *values,
            vec!["https://other.example/child.xml".to_owned()],
            "the cycle warning must name the child on its own host, without userinfo"
        );
    }

    #[test]
    fn a_child_sitemap_url_loses_its_userinfo_with_or_without_a_base() {
        let child = "http://user:s3cret@example.com/child.xml";
        assert_eq!(
            resolve_child_sitemap_url(false, "not a url", child).as_deref(),
            Some("http://example.com/child.xml")
        );
        assert_eq!(
            resolve_child_sitemap_url(true, "http://example.com/sitemap.xml", child).as_deref(),
            Some("http://example.com/child.xml")
        );
    }

    #[test]
    fn a_sitemap_loc_loses_its_userinfo() {
        let xml = r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>http://user:s3cret@example.com/a</loc></url></urlset>"#;
        let config = CrawlConfig::default();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).expect("the default filter compiles");
        let urls = collect_urlset_entries(
            "http://example.com/sitemap.xml",
            xml,
            &walk_context(&config, &client, &filter),
            None,
        );
        let found: Vec<&str> = urls.iter().map(|entry| entry.url.as_str()).collect();
        assert_eq!(found, vec!["http://example.com/a"]);
    }
}
