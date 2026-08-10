//! Link extraction and classification from HTML documents.

use tl::VDom;
use url::Url;

use crate::types::{LinkInfo, LinkType};

use super::get_attr;
use super::selectors::{SEL_A_HREF, SEL_BASE_HREF};

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

/// Extract all links from a parsed HTML document.
pub(crate) fn extract_links(dom: &VDom<'_>, base_url: &Url) -> Vec<LinkInfo> {
    let parser = dom.parser();

    let effective_base = dom
        .query_selector(SEL_BASE_HREF)
        .and_then(|mut iter| {
            iter.next()
                .and_then(|h| h.get(parser))
                .and_then(|n| n.as_tag())
                .and_then(|tag| get_attr(tag, "href"))
                // ~keep A `<base href>` is often site-relative (e.g. "/en/"); resolve it against
                // the document URL instead of requiring it to already be absolute.
                .and_then(|href| base_url.join(href).ok())
        })
        .unwrap_or_else(|| base_url.clone());

    let mut links = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_A_HREF) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };

            let href = get_attr(tag, "href").unwrap_or("").trim();
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
