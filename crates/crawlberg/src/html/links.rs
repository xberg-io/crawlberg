//! Link extraction and classification from HTML documents.

use tl::VDom;
use url::Url;

use crate::types::{LinkInfo, LinkType};

use super::selectors::SEL_A_HREF;
use super::{decode_attr_value, elements_named, get_attr};

/// Document file extensions used for link classification.
static DOCUMENT_EXTENSIONS: &[&str] = &[
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".rtf", ".csv", ".txt", ".zip",
    ".tar", ".gz", ".rar",
];

/// Classify a link as internal, external, anchor, or document.
pub(crate) fn classify_link(href: &str, base_url: &Url) -> LinkType {
    if href.starts_with('#') {
        return LinkType::Anchor;
    }

    let lower = href.to_lowercase();
    for ext in DOCUMENT_EXTENSIONS {
        if lower.ends_with(ext) {
            return LinkType::Document;
        }
    }

    if let Ok(resolved) = base_url.join(href) {
        if resolved.host_str() != base_url.host_str() {
            return LinkType::External;
        }
        LinkType::Internal
    } else if href.starts_with("http://") || href.starts_with("https://") {
        if let Ok(u) = Url::parse(href)
            && u.host_str() != base_url.host_str()
        {
            return LinkType::External;
        }
        LinkType::Internal
    } else {
        LinkType::Internal
    }
}

/// The URL a document's relative references resolve against: the `href` of its first `<base>`
/// that has one, decoded and joined to the document URL, or the document URL itself.
pub(super) fn effective_base_url(dom: &VDom<'_>, document_url: &Url) -> Url {
    elements_named(dom, "base")
        .find_map(|tag| tag.attributes().get("href"))
        .map(|value| value.and_then(|v| v.try_as_utf8_str()).unwrap_or(""))
        // ~keep A `<base href>` is often site-relative (e.g. "/en/"); resolve it against
        // the document URL instead of requiring it to already be absolute.
        .and_then(|href| document_url.join(&decode_attr_value(href)).ok())
        .unwrap_or_else(|| document_url.clone())
}

/// Extract all links from a parsed HTML document.
pub(crate) fn extract_links(dom: &VDom<'_>, base_url: &Url) -> Vec<LinkInfo> {
    let parser = dom.parser();
    let effective_base = effective_base_url(dom, base_url);

    let mut links = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_A_HREF) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };

            // ~keep HTML strips only ASCII whitespace from a URL attribute. U+00A0, U+2000-200A,
            // U+3000 and U+0085 belong to the value and are percent-encoded, so the Unicode-aware
            // `str::trim` reported an address the browser and the markdown rewrite do not use.
            let href = get_attr(tag, "href")
                .unwrap_or("")
                .trim_matches(|c: char| c.is_ascii_whitespace());
            if href.is_empty() {
                continue;
            }

            if href.starts_with("mailto:")
                || href.starts_with("javascript:")
                || href.starts_with("tel:")
                || href.starts_with("data:")
            {
                continue;
            }

            // ~keep `Url::join` already resolves protocol-relative ("//host/path") references
            // per the WHATWG URL spec, so no special-casing is needed here.
            let link_type = classify_link(href, &effective_base);

            let resolved_url = if let Ok(u) = effective_base.join(href) {
                u.to_string()
            } else {
                href.to_owned()
            };

            let rel = get_attr(tag, "rel").map(String::from);
            let nofollow = rel.as_ref().map(|r| r.contains("nofollow")).unwrap_or(false);
            let text = tag.inner_text(parser).trim().to_owned();

            links.push(LinkInfo {
                url: resolved_url,
                text,
                link_type,
                rel,
                nofollow,
            });
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use tl::ParserOptions;

    use super::*;

    fn extract(html: &str, base: &str) -> Vec<LinkInfo> {
        let dom = tl::parse(html, ParserOptions::default()).expect("valid HTML");
        let base_url = Url::parse(base).expect("valid base URL");
        extract_links(&dom, &base_url)
    }

    #[test]
    fn resolves_protocol_relative_urls_to_the_base_scheme() {
        let html = r#"<a href="//cdn.example/x.js">script</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(
            links[0].url, "https://cdn.example/x.js",
            "protocol-relative URL should inherit the base scheme, got {}",
            links[0].url
        );
    }

    #[test]
    fn resolves_relative_base_href_against_the_document_url() {
        let html = r#"<base href="/en/"><a href="page.html">link</a>"#;
        let links = extract(html, "https://example.com/us/index.html");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(
            links[0].url, "https://example.com/en/page.html",
            "relative base href should resolve against the document URL, got {}",
            links[0].url
        );
    }

    #[test]
    fn should_extract_link_when_href_attribute_is_uppercase() {
        let html = r#"<a HREF="/docs/guide.html">guide</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "uppercase HREF should still match, got {links:?}");
        assert_eq!(
            links[0].url, "https://example.com/docs/guide.html",
            "uppercase HREF should resolve like a lowercase one, got {}",
            links[0].url
        );
    }

    #[test]
    fn should_read_rel_when_attribute_name_is_mixed_case() {
        let html = r#"<a href="https://other.example/" ReL="nofollow">out</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert!(
            links[0].nofollow,
            "mixed-case ReL=\"nofollow\" should be honoured, got rel={:?}",
            links[0].rel
        );
    }

    #[test]
    fn keeps_unicode_spaces_in_an_href_as_a_browser_does() {
        let html = "<a href=\"/a\u{a0}\">nbsp</a><a href=\"/b\u{3000}\">ideographic</a>";
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 2, "expected exactly two links, got {links:?}");
        assert_eq!(
            links[0].url, "https://example.com/a%C2%A0",
            "a trailing NBSP belongs to the address and must be percent-encoded, not trimmed, got {}",
            links[0].url
        );
        assert_eq!(
            links[1].url, "https://example.com/b%E3%80%80",
            "a trailing U+3000 belongs to the address and must be percent-encoded, not trimmed, got {}",
            links[1].url
        );
    }

    // ~keep Guard, not coverage: this already passes before the trim was narrowed to ASCII. It
    // pins that the narrowing still drops an all-ASCII-whitespace href, which would otherwise
    // resolve to the page URL and report the page as a link to itself.
    #[test]
    fn still_skips_an_href_that_is_only_ascii_whitespace() {
        let html = "<a href=\" \t\r\n \">blank</a>";
        let links = extract(html, "https://example.com/page");
        assert_eq!(
            links.len(),
            0,
            "an ASCII-whitespace-only href must be skipped, not resolved to the page URL, got {links:?}"
        );
    }

    #[test]
    fn classifies_fragment_only_href_as_anchor() {
        let base_url = Url::parse("https://example.com/page").expect("valid base URL");
        assert_eq!(
            classify_link("#section-1", &base_url),
            LinkType::Anchor,
            "a fragment-only href should classify as Anchor"
        );
    }

    #[test]
    fn classifies_pdf_extension_as_document() {
        let base_url = Url::parse("https://example.com/page").expect("valid base URL");
        assert_eq!(
            classify_link("/files/report.pdf", &base_url),
            LinkType::Document,
            "a PDF href should classify as Document"
        );
    }

    #[test]
    fn absolute_base_href_still_resolves_correctly() {
        let html = r#"<base href="https://cdn.example/assets/"><a href="img.png">img</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(
            links[0].url, "https://cdn.example/assets/img.png",
            "absolute base href should still resolve relative hrefs, got {}",
            links[0].url
        );
    }
}
