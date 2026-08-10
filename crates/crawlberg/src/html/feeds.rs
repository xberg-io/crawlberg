//! Feed, favicon, hreflang, and heading extraction from HTML documents.

use tl::VDom;
use url::Url;

use crate::types::{FaviconInfo, FeedInfo, FeedType, HeadingInfo, HreflangEntry};

use super::get_attr;
use super::resolve_url;
use super::selectors::{SEL_FAVICON, SEL_FEED_ALTERNATE, SEL_HEADINGS, SEL_HREFLANG};

/// Extract feed links (RSS, Atom, JSON Feed) from a parsed HTML document.
pub(crate) fn extract_feeds(dom: &VDom<'_>, base_url: &Url) -> Vec<FeedInfo> {
    let parser = dom.parser();
    let mut feeds = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_FEED_ALTERNATE) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            let link_type = get_attr(tag, "type").unwrap_or("");
            let raw_href = get_attr(tag, "href").unwrap_or("");
            let href = if raw_href.is_empty() {
                String::new()
            } else {
                resolve_url(raw_href, base_url)
            };
            let title = get_attr(tag, "title").map(String::from);

            let feed_type = match link_type {
                "application/rss+xml" => Some(FeedType::Rss),
                "application/atom+xml" => Some(FeedType::Atom),
                "application/json" | "application/feed+json" => Some(FeedType::JsonFeed),
                _ => None,
            };

            if let Some(ft) = feed_type {
                feeds.push(FeedInfo {
                    url: href,
                    title,
                    feed_type: ft,
                });
            }
        }
    }
    feeds
}

/// Extract hreflang alternate links from a parsed HTML document.
pub(crate) fn extract_hreflangs(dom: &VDom<'_>) -> Vec<HreflangEntry> {
    let parser = dom.parser();
    let mut entries = Vec::new();
    if let Some(iter) = dom.query_selector(SEL_HREFLANG) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            let lang = get_attr(tag, "hreflang").unwrap_or("").to_owned();
            let url = get_attr(tag, "href").unwrap_or("").to_owned();
            if !lang.is_empty() && !url.is_empty() {
                entries.push(HreflangEntry { lang, url });
            }
        }
    }
    entries
}

/// Icon `rel` values recognized as favicons.
const FAVICON_RELS: &[&str] = &["icon", "shortcut icon", "apple-touch-icon"];

/// Extract favicon and icon links from a parsed HTML document.
pub(crate) fn extract_favicons(dom: &VDom<'_>, base_url: &Url) -> Vec<FaviconInfo> {
    let parser = dom.parser();
    let mut favicons = Vec::new();
    // ~keep `tl`'s selector matcher does not reliably OR together multiple
    // `link[rel='x']` alternatives that share the same tag name (verified: a grouped
    // selector like `link[rel='icon'], link[rel='shortcut icon']` matches nothing even
    // though each alternative matches on its own). Select on attribute presence once
    // and filter the value in Rust instead of relying on the comma-grouped selector.
    if let Some(iter) = dom.query_selector(SEL_FAVICON) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            let rel = get_attr(tag, "rel").unwrap_or("");
            if !FAVICON_RELS.contains(&rel) {
                continue;
            }
            let raw_href = get_attr(tag, "href").unwrap_or("");
            if raw_href.is_empty() {
                continue;
            }
            let url = resolve_url(raw_href, base_url);
            let sizes = get_attr(tag, "sizes").map(String::from);
            let mime_type = get_attr(tag, "type").map(String::from);
            favicons.push(FaviconInfo {
                url,
                rel: rel.to_owned(),
                sizes,
                mime_type,
            });
        }
    }
    favicons
}

/// Extract heading elements (h1-h6) from a parsed HTML document.
pub(crate) fn extract_headings(dom: &VDom<'_>) -> Vec<HeadingInfo> {
    let parser = dom.parser();
    let mut headings = Vec::new();
    if let Some(iter) = dom.query_selector(SEL_HEADINGS) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            let tag_name = tag.name().as_utf8_str();
            let level = match tag_name.as_ref() {
                "h1" => 1,
                "h2" => 2,
                "h3" => 3,
                "h4" => 4,
                "h5" => 5,
                "h6" => 6,
                _ => continue,
            };
            let text = tag.inner_text(parser).trim().to_owned();
            headings.push(HeadingInfo { level, text });
        }
    }
    headings
}

#[cfg(test)]
mod tests {
    use tl::ParserOptions;

    use super::*;

    fn parse(html: &str) -> tl::VDom<'_> {
        tl::parse(html, ParserOptions::default()).expect("valid HTML")
    }

    #[test]
    fn resolves_relative_feed_urls_against_the_document_url() {
        let dom = parse(r#"<link rel="alternate" type="application/rss+xml" href="/feed.xml" title="Feed">"#);
        let base = Url::parse("https://example.com/blog/index.html").unwrap();
        let feeds = extract_feeds(&dom, &base);
        assert_eq!(feeds.len(), 1, "expected one feed, got {feeds:?}");
        assert_eq!(
            feeds[0].url, "https://example.com/feed.xml",
            "relative feed href should resolve against the document URL, got {}",
            feeds[0].url
        );
    }

    #[test]
    fn resolves_relative_favicon_urls_against_the_document_url() {
        let dom = parse(r#"<link rel="icon" href="favicon.ico">"#);
        let base = Url::parse("https://example.com/en/page.html").unwrap();
        let favicons = extract_favicons(&dom, &base);
        assert_eq!(favicons.len(), 1, "expected one favicon, got {favicons:?}");
        assert_eq!(
            favicons[0].url, "https://example.com/en/favicon.ico",
            "relative favicon href should resolve against the document URL, got {}",
            favicons[0].url
        );
    }

    #[test]
    fn finds_every_recognized_favicon_rel_variant() {
        let dom = parse(
            r#"<link rel="icon" href="a.ico">
               <link rel="shortcut icon" href="b.ico">
               <link rel="apple-touch-icon" href="c.png">
               <link rel="stylesheet" href="style.css">"#,
        );
        let base = Url::parse("https://example.com/").unwrap();
        let favicons = extract_favicons(&dom, &base);
        let rels: Vec<&str> = favicons.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec!["icon", "shortcut icon", "apple-touch-icon"],
            "expected all three favicon rel variants and no stylesheet link, got {favicons:?}"
        );
    }
}
