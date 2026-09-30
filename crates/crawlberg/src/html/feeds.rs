//! Feed, favicon, hreflang, and heading extraction from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{FaviconInfo, FeedInfo, FeedType, HeadingInfo, HreflangEntry};

use super::selectors::{SEL_HEADINGS, SEL_HREFLANG, SEL_LINK_REL};
use super::{fetchable_address, get_attr, get_url_attr, has_rel, is_fetchable_scheme, mime_essence};

/// Extract feed links (RSS, Atom, JSON Feed) from a parsed HTML document, resolved against the
/// document's base URL. A link with a blank `href`, or one whose address the crawler cannot
/// fetch, is skipped.
pub(crate) fn extract_feeds(dom: &VDom<'_>, base_url: &Url) -> Vec<FeedInfo> {
    let parser = dom.parser();
    let mut feeds = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_LINK_REL) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            if !has_rel(tag, "alternate") {
                continue;
            }
            let Some(href) = get_url_attr(tag, "href") else {
                continue;
            };
            let Some(href) = fetchable_address(&href, base_url) else {
                continue;
            };
            let link_type = mime_essence(tag).unwrap_or_default();
            let title = get_attr(tag, "title").map(Cow::into_owned);

            let feed_type = match link_type.as_str() {
                "application/rss+xml" => Some(FeedType::Rss),
                "application/atom+xml" => Some(FeedType::Atom),
                "application/json" | "application/feed+json" => Some(FeedType::JsonFeed),
                _ => None,
            };

            if let Some(ft) = feed_type {
                feeds.push(FeedInfo {
                    url: href.into(),
                    title,
                    feed_type: ft,
                });
            }
        }
    }
    feeds
}

/// Extract hreflang alternate links from a parsed HTML document, resolved against the document's
/// base URL. A link with a blank `hreflang` or `href`, or one whose address the crawler cannot
/// fetch, is skipped.
pub(crate) fn extract_hreflangs(dom: &VDom<'_>, base_url: &Url) -> Vec<HreflangEntry> {
    let parser = dom.parser();
    let mut entries = Vec::new();
    if let Some(iter) = dom.query_selector(SEL_HREFLANG) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            if !has_rel(tag, "alternate") {
                continue;
            }
            let lang = get_attr(tag, "hreflang").unwrap_or_default();
            let lang = lang.trim_ascii();
            if lang.is_empty() {
                continue;
            }
            let Some(href) = get_url_attr(tag, "href") else {
                continue;
            };
            let Some(url) = fetchable_address(&href, base_url) else {
                continue;
            };
            entries.push(HreflangEntry {
                lang: lang.to_owned(),
                url: url.into(),
            });
        }
    }
    entries
}

/// `rel` tokens recognized as favicons. `rel="shortcut icon"` holds the `icon` token.
const FAVICON_RELS: &[&str] = &["icon", "apple-touch-icon"];

/// Extract favicon and icon links from a parsed HTML document, resolved against the document's
/// base URL. A link with a blank `href`, or one whose address the crawler cannot fetch, is
/// skipped; an inline `data:` icon is kept, since it is a real, usable icon that needs no fetch,
/// unlike a `file:` or `blob:` address.
pub(crate) fn extract_favicons(dom: &VDom<'_>, base_url: &Url) -> Vec<FaviconInfo> {
    let parser = dom.parser();
    let mut favicons = Vec::new();
    if let Some(iter) = dom.query_selector(SEL_LINK_REL) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };
            if !FAVICON_RELS.iter().any(|token| has_rel(tag, token)) {
                continue;
            }
            let rel = get_attr(tag, "rel").unwrap_or_default();
            let Some(raw_href) = get_url_attr(tag, "href") else {
                continue;
            };
            let Some(url) = crate::net::userinfo::resolve(base_url, &raw_href)
                .filter(|url| is_fetchable_scheme(url) || url.scheme() == "data")
            else {
                continue;
            };
            let sizes = get_attr(tag, "sizes").map(Cow::into_owned);
            let mime_type = get_attr(tag, "type").map(Cow::into_owned);
            favicons.push(FaviconInfo {
                url: url.into(),
                rel: rel.into_owned(),
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
    use super::*;

    fn parse(html: &str) -> tl::VDom<'_> {
        crate::html::parse_html(html).expect("valid HTML")
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
    fn should_skip_a_favicon_whose_href_is_only_whitespace() {
        let dom = parse("<link rel=\"icon\" href=\" \t\r\n \">");
        let base = Url::parse("https://example.com/en/page.html").unwrap();
        let favicons = extract_favicons(&dom, &base);
        assert_eq!(
            favicons.len(),
            0,
            "a whitespace-only favicon href must not be reported as the page's own favicon, got {favicons:?}"
        );
    }

    #[test]
    fn should_keep_a_favicon_whose_href_is_only_a_unicode_space() {
        let dom = parse("<link rel=\"icon\" href=\"\u{a0}\">");
        let base = Url::parse("https://example.com/en/page.html").unwrap();
        let favicons = extract_favicons(&dom, &base);
        assert_eq!(favicons.len(), 1, "expected one favicon, got {favicons:?}");
        assert_eq!(
            favicons[0].url, "https://example.com/en/%C2%A0",
            "an NBSP-only href is part of the address and must be percent-encoded, not treated as blank, got {}",
            favicons[0].url
        );
    }

    #[test]
    fn skips_a_feed_with_a_file_or_blob_address() {
        let dom = parse(concat!(
            r#"<link rel="alternate" type="application/rss+xml" href="file:///etc/passwd">"#,
            r#"<link rel="alternate" type="application/atom+xml" href="blob:https://example.com/x">"#,
            r#"<link rel="alternate" type="application/rss+xml" href="feed.xml">"#,
        ));
        let base = Url::parse("https://example.com/").unwrap();
        let feeds = extract_feeds(&dom, &base);
        let urls: Vec<&str> = feeds.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://example.com/feed.xml"],
            "a file: or blob: feed link must be dropped, the crawler can never fetch it, got {feeds:?}"
        );
    }

    #[test]
    fn skips_a_hreflang_with_a_file_or_blob_address() {
        let dom = parse(concat!(
            r#"<link rel="alternate" hreflang="de" href="file:///etc/passwd">"#,
            r#"<link rel="alternate" hreflang="fr" href="blob:https://example.com/x">"#,
            r#"<link rel="alternate" hreflang="en" href="en.html">"#,
        ));
        let base = Url::parse("https://example.com/").unwrap();
        let entries = extract_hreflangs(&dom, &base);
        let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://example.com/en.html"],
            "a file: or blob: hreflang link must be dropped, the crawler can never fetch it, got {entries:?}"
        );
    }

    #[test]
    fn skips_a_favicon_with_a_file_or_blob_address_but_keeps_a_data_icon() {
        let dom = parse(concat!(
            r#"<link rel="icon" href="file:///etc/passwd">"#,
            r#"<link rel="icon" href="blob:https://example.com/x">"#,
            r#"<link rel="icon" href="data:image/png;base64,iVBORw0KGgo=">"#,
            r#"<link rel="icon" href="fav.ico">"#,
        ));
        let base = Url::parse("https://example.com/").unwrap();
        let favicons = extract_favicons(&dom, &base);
        let urls: Vec<&str> = favicons.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(
            urls,
            ["data:image/png;base64,iVBORw0KGgo=", "https://example.com/fav.ico"],
            "a file: or blob: icon must be dropped, the crawler can never fetch it, but a data: \
             icon is a real, usable icon and must be kept, got {favicons:?}"
        );
    }

    #[test]
    fn skips_a_feed_hreflang_or_icon_address_that_does_not_resolve() {
        let head = |href: &str| {
            format!(
                r#"<link rel="alternate" type="application/rss+xml" href="{href}">
                   <link rel="alternate" hreflang="de" href="{href}"><link rel="icon" href="{href}">"#
            )
        };
        let https = Url::parse("https://example.com/").unwrap();
        let blob = Url::parse("blob:https://example.com/b").unwrap();
        let cases = [
            ("file://[bad/x", &https),
            ("http://[bad/x", &https),
            ("data://[a", &https),
            ("feed.xml", &blob),
        ];
        for (href, base) in cases {
            let page = head(href);
            let dom = parse(&page);
            assert!(extract_feeds(&dom, base).is_empty(), "feed for {href:?} under {base}");
            assert!(
                extract_hreflangs(&dom, base).is_empty(),
                "hreflang for {href:?} under {base}"
            );
            assert!(
                extract_favicons(&dom, base).is_empty(),
                "icon for {href:?} under {base}"
            );
        }
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
